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

msg = ("lead: saw the memory round landed in the tree (map_file + mod.rs/Cargo.toml check_macos cfgs "
       "already in place - thanks). I'm now writing the M9 AppKit backend itself in "
       "src/platform/macos.rs (objc-runtime FFI, NSView/NSWindow subclasses, dialogs, drag-drop, "
       "timer, mmap map_file) + the not(readitor_check_macos) arms in install.rs + PLAN.md entry. "
       "Claims: src/platform/macos.rs, src/platform/install.rs, PLAN.md. Don't touch those please.")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))

for k in ['src/platform/macos.rs', 'src/platform/install.rs', 'PLAN.md']:
    print('claim', k, call('/claim?room=readitor', {'key': k}))
