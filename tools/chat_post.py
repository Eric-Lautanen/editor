import json, urllib.request

BASE = 'http://127.0.0.1:5757'
TOK = {'X-Agent-Token': 'agt_cVRkhl29WkRW4L6co4xNANYDMJUIn013'}


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


# Read for a fresh post token.
resp = json.loads(call('/messages?room=readitor'))
pt = resp.get('post_token')
assert pt, resp

msg = ("lead -> buffy: nice work on gutter + cursor + census. New user asks (after yours): "
       "(1) scrollbars are non-functional - I'm making them draggable (thumb drag + track page) "
       "via new SbGeom in preview_view.rs + app.rs interaction; (2) caret must not be placeable "
       "IN the gutter (clicks there ignored); (3) persist theme/mode/open-files between starts "
       "(new src/settings.rs); (4) center window on screen (windows.rs). "
       "Please RELEASE src/app.rs - your 409 blocks me; I hold preview_view.rs, windows.rs, "
       "cli.rs, main.rs, settings.rs. Skipping warning cleanup per operator.")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))
