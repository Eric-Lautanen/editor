import json, urllib.request

BASE = 'http://127.0.0.1:5757'
TOK = {'X-Agent-Token': 'agt_egImZghVAwH0oQ21bqUvuTHSottVRpjh'}


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
msg = ("lead: editor-UX round LANDED, 149/149 tests green, fmt clean, release exe built. "
       "(1) scrollbars now WORK: SbGeom shared by paint/cursor/interaction, thumb drag + track-click "
       "pages-then-grabs, V all modes + H editor pane; (2) caret can no longer enter/bleed under the "
       "gutter: content inset by rail (PaneScroll::content_x), rail clicks ignored, Arrow cursor there, "
       "follow_caret pans in content space; (3) single click = caret, double = word (char-class runs), "
       "triple = whole line, drag extends from selection start; (4) session persistence: settings.rs "
       "(theme/mode/tabs/active -> %APPDATA%/Readitor/session.txt, written on change, restored at start "
       "unless a CLI file is given); (5) window centers on primary monitor. Also fixed the mangled "
       "editor_view.rs tests + duplicate centering block in windows.rs from the concurrent-edit window. "
       "PLAN.md updated + docs synced. Releasing claims on app.rs, preview_view.rs, editor_view.rs, "
       "windows.rs, cli.rs, main.rs, settings.rs. Remaining: warning cleanup (operator deferred), "
       "RAM-audit live-process measurement, M8/M9, M10.")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))
for k in ['src/app.rs', 'src/ui/preview_view.rs', 'src/ui/editor_view.rs',
          'src/platform/windows.rs', 'src/cli.rs', 'src/main.rs', 'src/settings.rs']:
    print(call('/release?room=readitor', {'key': k}))
