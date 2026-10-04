#!/usr/bin/env bash
# Sweeps spike S1: every transport x frame size x rate, under Wayland and X11, and
# appends one JSON line per run to results-<host>.jsonl, then prints the summary.
# Usage: ./run.sh [seconds per run, default 8]
set -euo pipefail
cd "$(dirname "$0")"
SECONDS_PER_RUN=${1:-8}
OUT=results-$(hostname -s).jsonl
(cd src-tauri && cargo build --release)
BIN=src-tauri/target/release/s1-transport

# The display servers to try: both when a Wayland session also offers XWayland.
backends=()
[[ -n ${WAYLAND_DISPLAY:-} ]] && backends+=(wayland)
[[ -n ${DISPLAY:-} ]] && backends+=(x11)

gpu=$( (lspci 2>/dev/null | grep -Ei 'vga|3d' | head -1 | sed 's/^[^:]*: //') || true)
for backend in "${backends[@]}"; do
  for transport in scheme ipc ws; do
    for bytes in 1000000 3000000 5500000 12000000; do
      for fps in 0 30; do
        echo ">> $backend $transport $bytes bytes, fps ${fps/#0/max}" >&2
        line=$(GDK_BACKEND=$backend S1_TRANSPORT=$transport S1_BYTES=$bytes S1_FPS=$fps \
               S1_SECONDS=$SECONDS_PER_RUN timeout $((SECONDS_PER_RUN + 60)) "$BIN" 2>/dev/null | tail -1) \
          || line='{"error":"run failed or timed out"}'
        python3 - "$line" "$backend" "$transport" "$bytes" "$fps" "$gpu" >>"$OUT" <<'PY'
import json, sys
line, backend, transport, size, fps, gpu = sys.argv[1:]
try:
    r = json.loads(line)
except ValueError:
    r = {"error": line[:200]}
r.update(backend=backend, transport=transport, bytes=int(size), target_fps=float(fps), gpu=gpu)
print(json.dumps(r))
PY
      done
    done
  done
done
python3 summarize.py "$OUT"
