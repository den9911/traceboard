import http.server,threading,json,tempfile,pathlib,subprocess,os,time,urllib.request
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  code={'/ok':200,'/empty':204,'/missing':404,'/bad':500,'/redirect':302}.get(self.path,200)
  if self.path=='/slow':time.sleep(.1)
  if self.path=='/timeout':time.sleep(11)
  try:
   self.send_response(code)
   if self.path=='/redirect':self.send_header('Location','/ok')
   self.end_headers()
  except OSError:pass
 def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
base='http://127.0.0.1:'+str(server.server_port);binary=pathlib.Path('target/release/traceboard').resolve()
with tempfile.TemporaryDirectory() as d:
 root=pathlib.Path(d);targets=[{'name':p,'url':base+'/'+p}for p in ['ok','empty','missing','bad','redirect','slow','timeout']];targets[2]['expected_statuses']=[404];targets[5]['latency_warning_ms']=1
 (root/'targets.json').write_text(json.dumps({'targets':targets,'interval_seconds':3600}))
 env=dict(os.environ);env.pop('TELEGRAM_BOT_TOKEN',None);env.pop('TELEGRAM_CHAT_ID',None)
 proc=subprocess.Popen([str(binary)],cwd=root,env=env,stdout=subprocess.DEVNULL)
 try:
  for _ in range(180):
   try:
    with urllib.request.urlopen('http://127.0.0.1:3000/api/monitors',timeout=2)as r:data=json.load(r)
    if all(x['history']for x in data):break
   except OSError:pass
   time.sleep(.1)
  else:raise RuntimeError('history missing')
  values={x['name']:x['history'][-1]for x in data}
  assert all(values[n]['ok']for n in ['ok','empty','missing','redirect','slow']),values
  assert not values['bad']['ok']and not values['timeout']['ok']
  assert values['slow']['warning']and not values['slow']['error']
 finally:proc.terminate();proc.wait(timeout=5)
server.shutdown();server.server_close();print('PASS local HTTP: status rules, redirect, timeout and independent latency warning')
