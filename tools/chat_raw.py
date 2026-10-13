import json, urllib.request

BASE = 'http://127.0.0.1:5757'

r = json.loads(urllib.request.urlopen(urllib.request.Request(
    BASE + '/auth', data=json.dumps({"name": "lead", "room": "readitor", "takeover": True}).encode(),
    headers={'Content-Type': 'application/json'})).read().decode())
tok = {'X-Agent-Token': r['agent_token']}

req = urllib.request.Request(BASE + '/messages?room=readitor', headers=tok)
raw = urllib.request.urlopen(req).read().decode()
print(raw[:6000])
