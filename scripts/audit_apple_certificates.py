"""Read-only inventory of Apple certificate types; never print credentials."""
import base64
import contextlib
import io
import json
import os
import urllib.request
from testflight_distribute import make_token

with contextlib.redirect_stdout(io.StringIO()):
    token = make_token(os.environ['ASC_KEY_ID'], os.environ['ASC_ISSUER_ID'], base64.b64decode(os.environ['ASC_KEY_P8_BASE64']))
request = urllib.request.Request('https://api.appstoreconnect.apple.com/v1/certificates?limit=200', headers={'Authorization': 'Bearer ' + token})
with urllib.request.urlopen(request, timeout=60) as response:
    payload = json.load(response)
for item in payload.get('data', []):
    data = item['attributes']
    print(json.dumps({'type': data.get('certificateType'), 'expires': data.get('expirationDate')}))
