"""Real process interruption, durable cooldown, storage recovery and full-queue checks."""
import collections
import hashlib
import http.server
import json
import os
import pathlib
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request

binary = pathlib.Path('target/release/traceboard').resolve()
state = {'status':200, 'mode':'hold'}
seen = collections.defaultdict(list)
gate = threading.Event()
lock = threading.Lock()

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        self.send_response(state['status']); self.end_headers()
    def do_POST(self):
        value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        event = int(value['text'].rsplit('Event #',1)[-1])
        with lock:
            seen[event].append(time.monotonic()); attempt = len(seen[event]); mode = state['mode']
        if mode == 'hold': gate.wait(10)
        status = 200; reply = {'ok':True,'result':{'message_id':event}}
        if mode == 'rate' and attempt == 1:
            status = 429; reply = {'ok':False,'error_code':429,'parameters':{'retry_after':3}}
        body = json.dumps(reply).encode()
        try:
            self.send_response(status); self.send_header('Content-Length',str(len(body)))
            self.end_headers(); self.wfile.write(body)
        except OSError: pass

server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
threading.Thread(target=server.serve_forever,daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'

def get(path):
    with urllib.request.urlopen('http://127.0.0.1:3000'+path,timeout=1) as response: return json.load(response)
def wait(predicate, timeout=12):
    end = time.monotonic()+timeout
    while time.monotonic()<end:
        try:
            value=predicate()
            if value:return value
        except OSError:pass
        time.sleep(.04)
    raise AssertionError('Expected state did not arrive')
def refresh():
    request=urllib.request.Request('http://127.0.0.1:3000/api/refresh',data=b'',method='POST')
    with urllib.request.urlopen(request,timeout=2) as response: assert response.status==204

def overview(): return get('/api/notifications')
def count(db):
    with sqlite3.connect(db) as conn:return conn.execute('SELECT count(*) FROM checks').fetchone()[0]
def latest(db):
    with sqlite3.connect(db) as conn:return conn.execute('SELECT id,state,attempts,next_at FROM notification_outbox ORDER BY id DESC LIMIT 1').fetchone()

with tempfile.TemporaryDirectory(prefix='outbox recovery ') as directory:
    root=pathlib.Path(directory);db=root/'checks.sqlite';log=root/'process.log'
    (root/'targets.json').write_text(json.dumps({'targets':[{'name':'private service','url':base+'/target'}],'failure_threshold':1,'interval_seconds':3600}))
    env={**os.environ,'TRACEBOARD_DB':str(db),'TELEGRAM_BOT_TOKEN':'12345:fixture-token','TELEGRAM_CHAT_ID':'private-chat','TRACEBOARD_TELEGRAM_API':base}
    output=log.open('ab')
    def launch():
        before=count(db) if db.exists() else 0
        process=subprocess.Popen([str(binary)],cwd=root,env=env,stdout=output,stderr=output)
        wait(lambda:get('/api/monitors'))
        wait(lambda:count(db)>=before+1)
        return process
    process=launch()
    try:
        wait(lambda:count(db)==1)
        state['status']=500;refresh()
        wait(lambda:bool(seen))
        interrupted=latest(db)[0];assert latest(db)[1]=='sending'
        process.kill();process.wait(timeout=5);gate.set()
        state['mode']='success'
        process=launch()
        wait(lambda:overview()['reasons']['interrupted']==1)
        time.sleep(1.2);assert len(seen[interrupted])==1

        state.update(status=200,mode='rate');refresh()
        wait(lambda:latest(db)[1]=='pending' and latest(db)[2]==1)
        delayed=latest(db)[0];assert overview()['reasons']['rate_limited']==1
        process.kill();process.wait(timeout=5)
        process=launch()
        wait(lambda:latest(db)[1]=='delivered')
        assert len(seen[delayed])==2 and seen[delayed][1]-seen[delayed][0]>=3
        assert len(seen[interrupted])==1

        # Telegram accepted the request, but its local completion cannot be stored.
        with sqlite3.connect(db) as conn:
            conn.executescript("CREATE TRIGGER reject_finish BEFORE UPDATE OF state ON notification_outbox WHEN new.state='delivered' BEGIN SELECT RAISE(ABORT,'private completion fixture'); END;")
        state.update(status=500,mode='success');refresh()
        wait(lambda:overview()['sender_stopped'])
        accepted=latest(db)[0];assert latest(db)[1]=='sending' and len(seen[accepted])==1
        before=count(db);refresh();assert count(db)==before+1
        assert overview()['sender_stopped']
        with sqlite3.connect(db) as conn:conn.execute('DROP TRIGGER reject_finish')
        process.kill();process.wait(timeout=5);process=launch()
        wait(lambda:overview()['reasons']['interrupted']==2)
        time.sleep(1.2);assert len(seen[accepted])==1

        # A write failure is not cleared from diagnostics by unrelated claim polling.
        with sqlite3.connect(db) as conn:
            conn.executescript("CREATE TRIGGER reject_intent BEFORE INSERT ON notification_outbox BEGIN SELECT RAISE(ABORT,'private record fixture'); END;")
        before=count(db);state['status']=200;refresh()
        wait(lambda:overview()['check_storage_error'])
        time.sleep(1.2)
        assert count(db)==before and overview()['check_storage_error']
        assert get('/api/monitors')[0]['history'][-1]['status']==500
        with sqlite3.connect(db) as conn:conn.execute('DROP TRIGGER reject_intent')
        refresh();wait(lambda:not overview()['check_storage_error'])
        wait(lambda:latest(db)[1]=='delivered')

        # Seed bounded load locally; no real bot and no 256-message network flood.
        process.kill();process.wait(timeout=5)
        destination=hashlib.sha256(b'12345\0private-chat').hexdigest()
        with sqlite3.connect(db) as conn:
            conn.execute("DELETE FROM notification_outbox WHERE state IN ('delivered','failed')")
            existing=conn.execute('SELECT count(*) FROM notification_outbox').fetchone()[0]
            for n in range(256-existing):
                conn.execute("INSERT INTO notification_outbox(check_id,destination,text,state,reason) VALUES(?,?,?,'uncertain','fixture-private-reason')",(1000000+n,destination,'capacity fixture'))
        process=launch();wait(lambda:overview()['uncertain']==256)
        for code in [500,200,500]:state['status']=code;refresh()
        assert overview()['overflow']==3 and overview()['uncertain']==256
        assert overview()['reasons']['other']==254
        process.kill();process.wait(timeout=5);process=launch()
        wait(lambda:overview()['overflow']==3)
        with sqlite3.connect(db) as conn:
            assert conn.execute('SELECT count(*) FROM notification_outbox').fetchone()[0]==256
            assert conn.execute('PRAGMA integrity_check').fetchone()[0]=='ok'
        public=json.dumps(overview())
        for secret in ['private service','fixture-token','private-chat','fixture-private-reason',str(db)]:assert secret not in public
        output.flush();logs=log.read_text()
        for secret in ['fixture-token','private-chat','private completion fixture','private record fixture']:assert secret not in logs
    finally:
        gate.set()
        if process.poll() is None:process.kill();process.wait(timeout=5)
        output.close();server.shutdown();server.server_close()
print('PASS recovery: interrupted sending, durable cooldown, failed confirmation, continued checks, sticky write failure, overflow persistence and secret-free diagnostics')
