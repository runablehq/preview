#!/usr/bin/env bash
# The smoke scenarios from the design (section 8). Each asserts the JSON or the message, then
# stops what it started. Used by ci.yml against a fresh build and by release.yml against the
# package installed from npm.
#
#   PREVIEW=/path/to/preview tests/smoke.sh     # default: preview on PATH
#   SMOKE_VITE=1 tests/smoke.sh                 # also the two-Vite-apps scenario (needs npm)
#
# Needs bash, python3, node and curl. Uses ports 8765–8768 (and 5173+ with SMOKE_VITE).

# The $PORT and $HOST in single quotes are for preview to fill in, not this shell.
# shellcheck disable=SC2016

set -u

PREVIEW=${PREVIEW:-preview}
SMOKE_HTTP_SERVER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/http-server.py"
export SMOKE_HTTP_SERVER
WORK=$(mktemp -d "${TMPDIR:-/tmp}/preview-smoke.XXXXXX")
export PREVIEW_HOME="$WORK/home"
# The binary under test, never a newer one it would update to. The update scenarios turn
# this back on against a fake registry.
export PREVIEW_UPDATE=0
REGISTRY_PID=""
FAILED=0
PASSED=0

cleanup() {
  for name in $("$PREVIEW" list | python3 -c 'import json,sys; print(" ".join(p["name"] for p in json.load(sys.stdin) if p["running"]))'); do
    "$PREVIEW" stop "$name" >/dev/null
  done
  [ -n "$REGISTRY_PID" ] && kill "$REGISTRY_PID" 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

field() { python3 -c 'import json,sys; print(json.loads(sys.argv[1])[sys.argv[2]])' "$1" "$2"; }
# A zombie counts as gone: a box without an init process never reaps it.
alive() { ps -o stat= -p "$1" 2>/dev/null | grep -qv '^Z'; }
http_ok() { [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 "http://127.0.0.1:$1/")" = 200 ]; }
listening() { curl -s -o /dev/null --max-time 2 "http://127.0.0.1:$1/"; }

ok() { PASSED=$((PASSED + 1)); echo "ok   $1"; }
bad() {
  FAILED=$((FAILED + 1))
  echo "FAIL $1"
  shift
  printf '     %s\n' "$@"
}

# preview_in <dir> <args...>: runs preview in a folder of $WORK; sets OUT, ERR, CODE.
preview_in() {
  local dir="$WORK/$1"
  shift
  mkdir -p "$dir"
  OUT=$(cd "$dir" && "$PREVIEW" "$@" 2>"$WORK/stderr")
  CODE=$?
  ERR=$(cat "$WORK/stderr")
}

NODE_SERVER="require('http').createServer((q,s)=>s.end('ok')).listen(process.env.PORT,process.env.HOST,()=>console.log('listening on '+process.env.PORT))"

echo "# $("$PREVIEW" --version) on $(uname -sm)"

# Reads PORT
preview_in node run -- node -e "$NODE_SERVER"
if [ "$CODE" = 0 ] && PORT1=$(field "$OUT" port) && PID1=$(field "$OUT" pid) && http_ok "$PORT1" &&
  grep -q "listening on $PORT1" "$(field "$OUT" log)"; then
  ok "reads PORT ($PORT1)"
else
  bad "reads PORT" "exit $CODE" "$OUT" "$ERR"
  PORT1="" PID1=""
fi

# Restart keeps port
preview_in node run -- node -e "$NODE_SERVER"
if [ "$CODE" = 0 ] && [ "$(field "$OUT" port)" = "$PORT1" ] && [ "$(field "$OUT" pid)" != "$PID1" ] &&
  ! alive "$PID1" && http_ok "$PORT1"; then
  ok "restart keeps port, replaces pid"
else
  bad "restart keeps port" "exit $CODE, first port $PORT1 pid $PID1" "$OUT" "$ERR"
fi
NODE_NAME=$(field "$OUT" name 2>/dev/null)

# list
LIST=$("$PREVIEW" list)
if python3 -c 'import json,sys; l=json.loads(sys.argv[1]); p=[p for p in l if p["name"]==sys.argv[2]][0]; assert p["running"] and p["ports"]==[int(sys.argv[3])], p' "$LIST" "$NODE_NAME" "$PORT1"; then
  ok "list shows the running preview"
else
  bad "list" "$LIST"
fi

# logs
LOGS=$("$PREVIEW" logs "$NODE_NAME")
if echo "$LOGS" | grep -q "listening on $PORT1"; then ok "logs"; else bad "logs" "$LOGS"; fi
"$PREVIEW" logs no-such-preview >/dev/null 2>"$WORK/stderr"
if [ $? = 1 ] && grep -q "No preview named no-such-preview" "$WORK/stderr"; then
  ok "logs of an unknown name"
else
  bad "logs of an unknown name" "$(cat "$WORK/stderr")"
fi

# stop --port on a preview's port
STOP=$("$PREVIEW" stop --port "$PORT1")
if [ "$STOP" = "Stopped preview $NODE_NAME." ] && ! listening "$PORT1"; then
  ok "stop --port stops the preview holding it"
else
  bad "stop --port on a preview" "$STOP"
fi

# Hard-coded port
preview_in fixed1 run python3 "$SMOKE_HTTP_SERVER" 8765
FIXED1=$(field "$OUT" name 2>/dev/null)
if [ "$CODE" = 0 ] && [ "$(field "$OUT" port)" = 8765 ] && http_ok 8765; then
  ok "hard-coded port"
else
  bad "hard-coded port" "exit $CODE" "$OUT" "$ERR"
fi

# Clash
preview_in fixed2 run python3 "$SMOKE_HTTP_SERVER" 8765
if [ "$CODE" = 1 ] && echo "$ERR" | grep -q "held by preview $FIXED1" &&
  echo "$ERR" | grep -q "preview stop --port 8765"; then
  ok "clash names the holder"
else
  bad "clash" "exit $CODE" "$OUT" "$ERR"
fi
"$PREVIEW" stop "$FIXED1" >/dev/null

# $PORT expansion
preview_in expand run -- python3 "$SMOKE_HTTP_SERVER" '$PORT'
P=$(field "$OUT" port 2>/dev/null)
if [ "$CODE" = 0 ] && [ "$P" != 8000 ] && http_ok "$P"; then
  ok "\$PORT expansion ($P)"
else
  bad "\$PORT expansion" "exit $CODE" "$OUT" "$ERR"
fi
"$PREVIEW" stop "$(field "$OUT" name 2>/dev/null)" >/dev/null 2>&1

# bash features
preview_in bash run 'source /dev/null && python3 "$SMOKE_HTTP_SERVER" $PORT'
if [ "$CODE" = 0 ] && http_ok "$(field "$OUT" port)"; then
  ok "bash features"
else
  bad "bash features" "exit $CODE" "$OUT" "$ERR"
fi
"$PREVIEW" stop "$(field "$OUT" name 2>/dev/null)" >/dev/null 2>&1

# Loopback only. macOS defaults HOST to 127.0.0.1, so ask for 0.0.0.0 there.
OUT=$(mkdir -p "$WORK/loop" && cd "$WORK/loop" &&
  PREVIEW_HOST=0.0.0.0 "$PREVIEW" run python3 "$SMOKE_HTTP_SERVER" 8766 --bind 127.0.0.1 2>"$WORK/stderr")
CODE=$?
ERR=$(cat "$WORK/stderr")
if [ "$CODE" = 1 ] && echo "$ERR" | grep -q "Port 8766 listens only on 127.0.0.1" &&
  echo "$ERR" | grep -q "for example"; then
  ok "loopback only"
else
  bad "loopback only" "exit $CODE" "$OUT" "$ERR"
fi
"$PREVIEW" stop --port 8766 >/dev/null

# Early exit
preview_in early run -- sh -c 'echo boom; exit 3'
if [ "$CODE" = 1 ] && echo "$ERR" | grep -q "exited (code 3)" && echo "$ERR" | grep -q boom; then
  ok "early exit shows the log"
else
  bad "early exit" "exit $CODE" "$OUT" "$ERR"
fi

# Slow start
preview_in slow run --wait 2 -- sleep 30
SLOW=$(cd "$WORK/slow" && "$PREVIEW" list | python3 -c 'import json,sys; print([p["name"] for p in json.load(sys.stdin) if p["command"]=="sleep 30"][0])')
if [ "$CODE" = 2 ] && echo "$ERR" | grep -q "still starting after 2 s" && echo "$ERR" | grep -q "preview logs $SLOW"; then
  ok "slow start exits 2"
else
  bad "slow start" "exit $CODE" "$OUT" "$ERR"
fi
"$PREVIEW" stop "$SLOW" >/dev/null

# Detached server
preview_in detached run 'nohup python3 "$SMOKE_HTTP_SERVER" 8767 >/dev/null 2>&1 &'
NAME=$(field "$OUT" name 2>/dev/null)
if [ "$CODE" = 0 ] && [ "$(field "$OUT" port)" = 8767 ] && http_ok 8767; then
  "$PREVIEW" stop "$NAME" >/dev/null
  if ! listening 8767; then ok "detached server, stopped"; else bad "detached server" "still listening after stop"; fi
else
  bad "detached server" "exit $CODE" "$OUT" "$ERR"
fi

# stop --port on a server no preview started
python3 "$SMOKE_HTTP_SERVER" 8768 >/dev/null 2>&1 &
LOOSE=$!
for _ in $(seq 50); do listening 8768 && break; sleep 0.1; done
STOP=$("$PREVIEW" stop --port 8768)
if echo "$STOP" | grep -q "Stopped pid $LOOSE" && ! alive "$LOOSE"; then
  ok "stop --port stops a loose server, not its shell"
else
  bad "stop --port on a loose server" "$STOP"
  kill "$LOOSE" 2>/dev/null
fi
wait "$LOOSE" 2>/dev/null
STOP=$("$PREVIEW" stop --port 8768)
if [ "$STOP" = "Nothing listens on port 8768." ]; then ok "stop --port on a free port"; else bad "stop --port on a free port" "$STOP"; fi

# --port busy
python3 "$SMOKE_HTTP_SERVER" 8768 >/dev/null 2>&1 &
LOOSE=$!
for _ in $(seq 50); do listening 8768 && break; sleep 0.1; done
preview_in busy run --port 8768 -- python3 "$SMOKE_HTTP_SERVER" '$PORT'
if [ "$CODE" = 1 ] && echo "$ERR" | grep -q "Port 8768 is held by pid $LOOSE"; then
  ok "--port busy names the holder"
else
  bad "--port busy" "exit $CODE" "$OUT" "$ERR"
fi
kill "$LOOSE"
wait "$LOOSE" 2>/dev/null

# Vite moves port
if [ "${SMOKE_VITE:-0}" = 1 ]; then
  mkdir -p "$WORK/vite1"
  (cd "$WORK/vite1" && echo '{"private":true}' >package.json && npm install --silent --no-audit --no-fund vite@6 >/dev/null 2>&1 && echo '<h1>hi</h1>' >index.html)
  mkdir -p "$WORK/vite2" && cp "$WORK/vite1/package.json" "$WORK/vite1/index.html" "$WORK/vite2/" && ln -s "$WORK/vite1/node_modules" "$WORK/vite2/node_modules"
  preview_in vite1 run 'npx vite --host $HOST'
  V1=$(field "$OUT" port 2>/dev/null) C1=$CODE E1=$ERR
  preview_in vite2 run 'npx vite --host $HOST'
  V2=$(field "$OUT" port 2>/dev/null)
  if [ "$C1" = 0 ] && [ "$CODE" = 0 ] && [ -n "$V1" ] && [ "$V2" = $((V1 + 1)) ]; then
    ok "vite moves port ($V1, $V2)"
  else
    bad "vite moves port" "exit $C1 $CODE, ports $V1 $V2" "$E1" "$ERR"
  fi
fi

# Auto-update, against a fake registry on port 8769 and a home of its own.
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) PLATFORM=linux-x64 ;;
  Linux-aarch64 | Linux-arm64) PLATFORM=linux-arm64 ;;
  Darwin-x86_64) PLATFORM=darwin-x64 ;;
  Darwin-arm64) PLATFORM=darwin-arm64 ;;
esac
REG="$WORK/registry"
REG_URL=http://127.0.0.1:8769
FAKE="$(cd "$(dirname "$0")" && pwd)/registry.py"
mkdir -p "$REG"
(cd "$REG" && exec python3 "$SMOKE_HTTP_SERVER" 8769 --bind 127.0.0.1 >/dev/null 2>&1) &
REGISTRY_PID=$!
for _ in $(seq 50); do listening 8769 && break; sleep 0.1; done
UPHOME="$WORK/uphome"
mkdir -p "$UPHOME"
up() { HOME="$UPHOME" PREVIEW_UPDATE=1 PREVIEW_REGISTRY=$REG_URL "$PREVIEW" "$@"; }
# preview upgrade by the binary under test itself, not the fixture it would hand off to.
upgrade_here() { PREVIEW_HANDOFF=smoke up upgrade 2>&1; }
python3 "$FAKE" "$REG" publish "$PLATFORM" 9.9.8 "$REG_URL"
python3 "$FAKE" "$REG" publish "$PLATFORM" 9.9.9 "$REG_URL"

# Update: any command starts a background check, its own output untouched; the next run is the new version.
OUT=$(up list)
if python3 -c 'import json,sys; json.loads(sys.argv[1])' "$OUT"; then
  for _ in $(seq 100); do [ "$(cat "$UPHOME/.runable/preview/current" 2>/dev/null)" = 9.9.9 ] && break; sleep 0.2; done
  NEXT=$(up list)
  if [ "$NEXT" = "fixture 9.9.9" ]; then
    ok "update: installs latest in the background, next run hands off"
  else
    bad "update" "next run printed: $NEXT" "update.json: $(cat "$UPHOME/.runable/preview/update.json" 2>/dev/null)"
  fi
else
  bad "update" "first run's stdout changed: $OUT"
fi

# Rollback: latest moves back and the active version is deprecated.
python3 "$FAKE" "$REG" latest "$PLATFORM" 9.9.8
python3 "$FAKE" "$REG" deprecate "$PLATFORM" 9.9.9
OUT=$(upgrade_here)
NEXT=$(up list)
if echo "$OUT" | grep -q "Installed preview 9.9.8" && [ "$NEXT" = "fixture 9.9.8" ]; then
  ok "rollback to the previous version"
else
  bad "rollback" "upgrade: $OUT" "next run: $NEXT"
fi

# A corrupted download is refused and changes nothing.
python3 "$FAKE" "$REG" publish "$PLATFORM" 9.9.10 "$REG_URL"
python3 "$FAKE" "$REG" corrupt "$PLATFORM" 9.9.10
OUT=$(upgrade_here)
CODE=$?
if [ "$CODE" = 1 ] && echo "$OUT" | grep -q "sha512 mismatch" && [ "$(up list)" = "fixture 9.9.8" ] &&
  grep -q lastError "$UPHOME/.runable/preview/update.json"; then
  ok "update refuses a tarball that fails its integrity check"
else
  bad "integrity" "exit $CODE: $OUT"
fi

# Pin and opt-out.
PINNED=$(PREVIEW_VERSION=9.9.9 up list)
OFF=$(HOME="$UPHOME" PREVIEW_UPDATE=0 "$PREVIEW" --version)
if [ "$PINNED" = "fixture 9.9.9" ] && echo "$OFF" | grep -q "updates off"; then
  ok "PREVIEW_VERSION pins, PREVIEW_UPDATE=0 stays put"
else
  bad "pin and opt-out" "pinned: $PINNED" "off: $OFF"
fi

echo "# $PASSED passed, $FAILED failed"
[ "$FAILED" = 0 ]
