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
msg = ("lead: cross-OS readiness round landed per operator: (1) .github/workflows/ci.yml - cargo test+clippy "
       "on windows/ubuntu/macos every push (suite is headless so the portable core runs on all 3), fmt check; "
       "(2) .github/workflows/release.yml - tag push builds+tests all 3 OSes, packages win zip / linux tar.gz "
       "with install.sh / macos .app zip, publishes a GitHub Release with install+update notes; (3) file "
       "associations documented for all 3 (win --install HKCU, linux install.sh desktop+mime+xdg-mime, macos "
       "CFBundleDocumentTypes); (4) update story = versioned overwrite, no re-register unless the exe moves; "
       "(5) mem-census is now a real cargo feature so --mem-report works. README + PLAN.md updated, docs synced. "
       "M8 (X11) / M9 (AppKit) backend implementation remains the big open item for real Linux/macOS windows.")
print(call('/send?room=readitor', {'message': msg}, {'X-Post-Token': pt}))
