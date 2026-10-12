import json, urllib.request

BASE = 'http://127.0.0.1:5757'
TOK = {'X-Agent-Token': 'agt_cVRkhl29WkRW4L6co4xNANYDMJUIn013'}


def post(path, data, headers=None):
    h = dict(TOK)
    if headers:
        h.update(headers)
    req = urllib.request.Request(BASE + path, data=json.dumps(data).encode(), headers=h)
    try:
        print(path, urllib.request.urlopen(req).read().decode()[:400])
    except Exception as e:
        print(path, 'ERR', e)


for k in ['src/app.rs', 'src/ui/preview_view.rs', 'src/platform/windows.rs',
          'src/cli.rs', 'src/main.rs', 'src/settings.rs']:
    post('/claim?room=readitor', {'key': k})
