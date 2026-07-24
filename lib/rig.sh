# lib/rig.sh — fetch the prebuilt bench "rig" (mock + loadgen) so a bench box needs NO build
# toolchain: bare OS + docker is enough. Downloads mock-<arch> / ugen-<arch> from the benchmarking
# `rig` GitHub release (rebuilt by .github/workflows/bench-rig.yml on every mock/ or loadgen/ change,
# for both arm64 and x86). Falls back to building from source ONLY if the download fails AND the
# toolchain happens to be present (local dev). Sets $MOCK and $UGEN. Idempotent — cached under bin/.
RIG_URL="${RIG_URL:-https://github.com/GetBusbar/benchmarking/releases/download/rig}"
fetch_rig() { # <repo-root>
  local root="$1" arch="${BENCH_ARCH:-arm64}"
  mkdir -p "$root/bin"
  MOCK="$root/bin/mock"; UGEN="$root/bin/ugen"
  if [ ! -x "$MOCK" ]; then
    if curl -fsSL "$RIG_URL/mock-$arch" -o "$MOCK" 2>/dev/null && [ -s "$MOCK" ]; then chmod +x "$MOCK"
    elif ( cd "$root/mock" && cargo build --release >/dev/null 2>&1 ); then cp "$root/mock/target/release/mock" "$MOCK"
    else echo "rig: cannot get mock ($RIG_URL/mock-$arch) and no rust toolchain to build it"; return 1; fi
  fi
  if [ ! -x "$UGEN" ]; then
    if curl -fsSL "$RIG_URL/ugen-$arch" -o "$UGEN" 2>/dev/null && [ -s "$UGEN" ]; then chmod +x "$UGEN"
    elif go build -o "$UGEN" "$root/loadgen/ugen.go" 2>/dev/null; then :
    else echo "rig: cannot get ugen ($RIG_URL/ugen-$arch) and no go toolchain to build it"; return 1; fi
  fi
}
