#!/usr/bin/env bash
# Run against a dedicated Compose project and a free localhost port.
set -euo pipefail
project="gamesync-smoke-$$"
appdata=$(mktemp -d)
export GAMESYNC_HOST_PORT="${GAMESYNC_HOST_PORT:-18787}"
compose=(docker compose -p "$project" -f docker/compose.yaml)
trap '"${compose[@]}" down -v; sudo rm -rf -- "$appdata"' EXIT
url="http://127.0.0.1:$GAMESYNC_HOST_PORT"

"${compose[@]}" up -d --build --wait
test_container=(docker run --rm --network none --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add FOWNER --cap-add SETUID --cap-add SETGID -v "$appdata:/var/lib/gamesync" gamesync-server:local)
printf 'existing data' > "$appdata/existing"
if "${test_container[@]}" invite --admin; then
  echo 'Expected initialization to refuse a non-empty inaccessible directory' >&2
  exit 1
fi
test "$(cat "$appdata/existing")" = 'existing data'
rm "$appdata/existing"
"${test_container[@]}" invite --admin > /dev/null
test -n "$(sudo ls -A "$appdata")"
invite=$("${compose[@]}" exec -T gamesync gamesync-server invite --admin)
credentials=$(curl -fsS "$url/v1/pair" -H 'Content-Type: application/json' \
  --data "{\"code\":\"$invite\",\"name\":\"Compose smoke test\"}")
token=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])' <<< "$credentials")
object_hash=af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
curl -fsS -X PUT "$url/v1/objects/$object_hash" \
  -H "Authorization: Bearer $token" --data-binary ''
curl -fsS -X PUT "$url/v1/retention" -H "Authorization: Bearer $token" \
  -H 'Content-Type: application/json' --data '{"keep_count":25,"keep_days":45}'
"${compose[@]}" up -d --force-recreate --wait
curl -fsS "$url/v1/objects/$object_hash/0" -H "Authorization: Bearer $token"
curl -fsS "$url/v1/status" -H "Authorization: Bearer $token" |
  python3 -c 'import json,sys; status=json.load(sys.stdin); assert (status["keep_count"], status["keep_days"]) == (25,45)'
echo 'Compose health, pairing, persistent objects and retention verified'
