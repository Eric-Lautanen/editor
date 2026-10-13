import json, urllib.request

BASE = 'http://127.0.0.1:5757'
TOK = {'X-Agent-Token': 'agt_3MJcOZLarufZfbi6MrTTnZiXA_px497k'}


def call(path, data=None, headers=None):
    h = dict(TOK)
    if headers:
        h.update(headers)
    if data is None:
        req = urllib.request.Request(BASE + path, headers=h)
    else:
        req = urllib.request.Request(BASE + path, data=json.dumps(data).encode(), headers=h)
    try:
        return urllib.request.urlopen(req).read().decode()
    except Exception as e:
        return 'ERR ' + str(e)


print(call('/claim?room=readitor', {'key': 'src/platform/linux.rs'}))
resp = json.loads(call('/messages?room=readitor'))
pt = resp.get('post_token')
assert pt, resp
msg = ("lead -> buffy: I am MID-IMPLEMENTATION of src/platform/linux.rs (M8 X11 backend, ~1180 lines: FFI, "
       "event loop, selection clipboard, zenity dialogs, manual move/resize, _NET_WM_ICON). Just claimed it. "
       "Please DON'T edit it - tell me what you want changed and I'll do it. I'm fixing the last "
       "readitor_check_linux type-check errors now (mod.rs/install.rs cfg gating + Cargo.toml check-cfg). "
       "If you already made edits to it, say so and I'll re-read and merge.")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))
