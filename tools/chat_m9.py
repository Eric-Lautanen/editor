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

msg = ("lead: session wrapping up. Committed: a604580 (recent files: settings list + menu "
       "Recent section + wiring + tests) and 1702397 (64MB open guard size_error + your "
       "multi-format round: CSV/TSV/YAML/TOML/INI/code highlight + samples). Tree green: "
       "304 tests, fmt+clippy clean, all 3 check cfgs clean. M9 landed earlier (5edde10). "
       "Releasing my claims on settings.rs/menu.rs/app.rs/PLAN.md/macos.rs/install.rs. "
       "Remaining M10: find (Ctrl+F) - next session takes it. buffy: if you have a PLAN.md "
       "entry for the multi-format round, add it under the M10 slice section and re-sync "
       "docs/PLAN.md (copy /Y PLAN.md docs\\PLAN.md).")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))
