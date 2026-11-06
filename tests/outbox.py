"""Local fake Telegram only: real server, SQLite, burst delivery and conservative retries."""
import collections
import http.server
import json
import os
import pathlib
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request

binary = pathlib.Path('target/release/traceboard').resolve()
lock = threading.Lock()
statuses = [200] * 4
requests = []
mode = 'hold'
gate = threading.Event()
rate_attempts = collections.Counter()

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        with lock:
            status = statuses[int(self.path.rsplit('/', 1)[-1])]
        self.send_response(status); self.end_headers()
    def do_POST(self):
        assert self.path == '/bot12345:fixture-token/sendMessage'
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        event = body['text'].rsplit('Event #', 1)[-1]
        with lock:
            requests.append((event, time.monotonic(), body))
            selected = mode
            rate_attempts[event] += 1
            attempt = rate_attempts[event]
        if selected == 'hold':
            gate.wait(timeout=8)
        if selected == 'unknown':
            self.connection.shutdown(socket.SHUT_RDWR)
            self.connection.close()
            return
        status = 200
        reply = {'ok': True, 'result': {'message_id': int(event)}}
        if selected == 'reject':
            reply = {'ok': False, 'error_code': 403, 'description': 'private failure text'}
        if selected == 'rate' and attempt == 1:
            status = 429
            reply = {'ok': False, 'error_code': 429, 'parameters': {'retry_after': 2}}
        encoded = json.dumps(reply).encode()
        try:
            self.send_response(status)
            self.send_header('Content-Length', str(len(encoded)))
            self.end_headers(); self.wfile.write(encoded)
        except OSError:
            pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'

def get(path):
    with urllib.request.urlopen('http://127.0.0.1:3000'+path, timeout=1) as response:
        return json.load(response)

def wait_for(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except OSError:
            pass
        time.sleep(.05)
    raise AssertionError('Expected state did not arrive')

def refresh():
    started = time.monotonic()
    with urllib.request.urlopen(urllib.request.Request(
        'http://127.0.0.1:3000/api/refresh', data=b'', method='POST'), timeout=2) as response:
        assert response.status == 204
    assert time.monotonic() - started < 2

def launch(root, env):
    process = subprocess.Popen([str(binary)], cwd=root, env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    wait_for(lambda: get('/api/monitors'))
    return process

try:
    with tempfile.TemporaryDirectory(prefix='trace outbox ') as directory:
        root = pathlib.Path(directory)
        db = root/'history.sqlite'
        config = {'targets': [{'name':f'private target {i}', 'url':f'{base}/target/{i}'} for i in range(4)],
                  'interval_seconds':3600, 'failure_threshold':1}
        (root/'targets.json').write_text(json.dumps(config))
        env = {**os.environ, 'TRACEBOARD_DB':str(db), 'TELEGRAM_BOT_TOKEN':'12345:fixture-token',
               'TELEGRAM_CHAT_ID':'private-chat-fixture', 'TRACEBOARD_TELEGRAM_API':base}
        process = launch(root, env)
        try:
            wait_for(lambda: all(m['history'] for m in get('/api/monitors')))
            with lock:
                statuses[:] = [500] * 4
            refresh()
            wait_for(lambda: get('/api/notifications')['sending'] == 1)
            overview = get('/api/notifications')
            assert overview['pending'] == 3 and overview['capacity'] == 256, overview
            assert all(m['history'][-1]['status'] == 500 for m in get('/api/monitors'))
            with lock:
                statuses[:] = [200] * 4
            refresh()  # History continues to commit while the HTTP notification is blocked.
            assert get('/api/notifications')['pending'] == 7
            gate.set()
            wait_for(lambda: get('/api/notifications')['delivered'] == 8, timeout=20)
            with lock:
                assert len(requests) == 8 and len({r[0] for r in requests}) == 8
                mode = 'reject'; statuses[0] = 500
            refresh()
            wait_for(lambda: get('/api/notifications')['failed'] == 1)
            with lock:
                rejected = requests[-1][0]
            time.sleep(1.2)
            with lock:
                assert rate_attempts[rejected] == 1
                mode = 'rate'; statuses[0] = 200
            refresh()
            wait_for(lambda: get('/api/notifications')['delivered'] == 9)
            with lock:
                event = requests[-1][0]
                timings = [r[1] for r in requests if r[0] == event]
                assert len(timings) == 2 and timings[1] - timings[0] >= 1.9
                mode = 'unknown'; statuses[0] = 500
            refresh()
            wait_for(lambda: get('/api/notifications')['uncertain'] == 1)
            with lock:
                unknown = requests[-1][0]
            time.sleep(1.2)
            assert rate_attempts[unknown] == 1
            overview = get('/api/notifications')
            text = json.dumps(overview)
            for private in ['fixture-token', 'private-chat-fixture', 'private target', 'private failure text', str(db)]:
                assert private not in text
            with sqlite3.connect(db) as conn:
                assert conn.execute("SELECT count(*) FROM notification_outbox WHERE state='uncertain'").fetchone()[0] == 1
                assert conn.execute('SELECT count(*) FROM checks').fetchone()[0] >= 24
            process.kill(); process.wait(timeout=5)
            process = launch(root, env)
            wait_for(lambda: get('/api/notifications')['uncertain'] == 1)
            time.sleep(1.5)
            assert rate_attempts[unknown] == 1  # Restart does not blindly send it again.
            for file in root.glob('history.sqlite*'):
                assert b'fixture-token' not in file.read_bytes()
                assert b'private-chat-fixture' not in file.read_bytes()
        finally:
            if process.poll() is None:
                process.kill(); process.wait(timeout=5)
finally:
    gate.set()
    server.shutdown(); server.server_close()
print('PASS fake Telegram: burst queue, independent history, HTTP200 ok=false, bounded 429 retry, unknown delivery without replay and secret-free overview')
