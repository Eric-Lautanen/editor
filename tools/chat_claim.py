import json, sys, urllib.request

BASE = 'http://127.0.0.1:5757'

def auth():
    r = json.loads(urllib.request.urlopen(urllib.request.Request(
        BASE + '/auth', data=json.dumps({"name": "lead", "room": "readitor", "takeover": True}).encode(),
        headers={'Content-Type': 'application/json'})).read().decode())
    return {'X-Agent-Token': r['agent_token']}

def call(path, data=None, headers=None):
    h = dict(headers or {})
    if data is None:
        req = urllib.request.Request(BASE + path, headers=h)
    else:
        req = urllib.request.Request(BASE + path, data=json.dumps(data).encode(), headers=h)
    return urllib.request.urlopen(req).read().decode()

tok = auth()
keys = sys.argv[1:]
for k in keys:
    try:
        print(k, '->', call('/claim?room=readitor', {'key': k}, tok))
    except urllib.error.HTTPError as e:
        print(k, '-> HTTP', e.code, e.read().decode()[:300])
