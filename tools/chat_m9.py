import json, urllib.request

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
pt = resp.get('post_token')

msg = ("lead: starting the M10 'recent files' slice: settings.rs gains a capped recent list, "
       "menu.rs gains a recent section, app.rs wires open/save -> recent + menu Recent(i) -> open. "
       "CLAIMING src/settings.rs, src/ui/menu.rs, src/app.rs - buffy stay out of those three "
       "until I post again (find/large-file come after, I'll re-claim then).")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))

for k in ['src/settings.rs', 'src/ui/menu.rs', 'src/app.rs']:
    print('claim', k, call('/claim?room=readitor', {'key': k}))
