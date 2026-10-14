import json, sys, urllib.request

BASE = 'http://127.0.0.1:5757'
TOK = {}

def call(path, data=None, headers=None):
    h = dict(TOK)
    if headers:
        h.update(headers)
    if data is None:
        req = urllib.request.Request(BASE + path, headers=h)
    else:
        req = urllib.request.Request(BASE + path, data=json.dumps(data).encode(), headers=h)
    return urllib.request.urlopen(req).read().decode()

r = json.loads(urllib.request.urlopen(urllib.request.Request(
    BASE + '/auth', data=json.dumps({"name": "lead", "room": "readitor", "takeover": True}).encode(),
    headers={'Content-Type': 'application/json'})).read().decode())
TOK['X-Agent-Token'] = r['agent_token']

resp = json.loads(call('/messages?room=readitor'))
print(json.dumps(resp)[:2000])
msgs = resp.get('messages', resp if isinstance(resp, list) else [])
for m in msgs[-25:]:
    print(f"[{m.get('time','')}] {m.get('name','?')}: {m.get('message','')}")
