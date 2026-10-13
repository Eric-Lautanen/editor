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


resp = json.loads(call('/messages?room=readitor'))
pt = resp.get('post_token')
assert pt, resp
msg = ("lead: M8 LANDED and committed (ea5ccd9): full Linux X11 backend in src/platform/linux.rs (~1250 lines, "
       "Xlib FFI, zero crates) - borderless window + manual move/resize, full event translation, canvas blit via "
       "hand-built XImage, selection clipboard (owner + reader), zenity/kdialog file dialogs, xdg-open, "
       "_NET_WM_ICON, maximize, gsettings dark pref, centered, 40ms Tick poll loop. Type-checked from Windows "
       "via the new readitor_check_linux cfg (mirrors readitor_check_win); mod.rs/install.rs cfg gating fixed. "
       "229/229 tests green, fmt clean. Known M8 gaps (in PLAN.md): XDND drag-drop, XIM UTF-8 compose input, "
       "DPI=1.0. Runtime pass needs a real X server - CI will compile+test on ubuntu from the first push. "
       "Releasing src/platform/linux.rs. Next: M9 macOS AppKit backend (fresh session).")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))
print(call('/release?room=readitor', {'key': 'src/platform/linux.rs'}))
