#!/bin/sh
# Verify a Skyward build for a device the way this adaptation was verified:
# the suite and the workflow's own commands, the stock recovery the image is
# built on, the packed image's structure, and — with the phone's own
# partitions to hand — every path and module the configuration names.
#
#   tools/verify.sh --config devices/ishtar.toml
#   tools/verify.sh --config devices/ishtar.toml --firmware /path/to/parts
#
# The firmware directory holds partitions taken out of the phone's package and
# mounted (or unpacked): vendor/, odm/, vendor_dlkm/, system_dlkm/. Root is
# needed to mount them; nothing here writes to a phone.
set -u

config=devices/ishtar.toml
firmware=""
image=""
while [ $# -gt 0 ]; do
	case "$1" in
		--config) config="$2"; shift 2 ;;
		--firmware) firmware="$2"; shift 2 ;;
		--image) image="$2"; shift 2 ;;
		-h|--help) sed -n '2,12p' "$0" | sed 's/^# \?//'; exit 0 ;;
		*) echo "unknown argument: $1" >&2; exit 2 ;;
	esac
done

[ -f "$config" ] || { echo "no configuration at $config" >&2; exit 2; }

# rustup keeps cargo in a folder of its own, which a shell that has not read
# its profile yet does not have.
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
	. "$HOME/.cargo/env"
fi

passed=0
failed=0
skipped=0
say() { printf '%-46s %s\n' "$1" "$2"; }
pass() { passed=$((passed + 1)); say "$1" "ok"; }
fail() { failed=$((failed + 1)); say "$1" "FAILED"; }
skip() { skipped=$((skipped + 1)); say "$1" "skipped: $2"; }

echo "device configuration: $config"
echo

# 1. The suite and the commands the workflow runs, with the flags it uses.
run() {
	name="$1"
	shift
	if "$@" >/tmp/verify.$$ 2>&1; then
		pass "$name"
	else
		fail "$name"
		sed -n '1,20p' /tmp/verify.$$ | sed 's/^/    /'
	fi
	rm -f /tmp/verify.$$
}

run "cargo test --workspace --locked" cargo test --workspace --locked -q
run "cargo clippy --workspace --all-targets -- -D warnings" cargo clippy --workspace --all-targets --locked -q -- -D warnings

# 2. The stock recovery the image is built on: its SHA-256 has to be the one
#    the configuration names.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if packer=$(ls target/release/skyward-pack target/debug/skyward-pack 2>/dev/null | head -n 1) && [ -n "$packer" ]; then
	if "$packer" stock --config "$config" --out "$work/recovery.img" >/tmp/stock.$$ 2>&1; then
		pass "the stock recovery's SHA-256 is the one named"
	else
		fail "the stock recovery's SHA-256 is the one named"
		sed -n '1,12p' /tmp/stock.$$ | sed 's/^/    /'
	fi
	rm -f /tmp/stock.$$
else
	skip "the stock recovery's SHA-256" "build skyward-pack first"
fi

# 3. The packed image's structure, if one was given.
if [ -n "$image" ]; then
	if command -v python3 >/dev/null 2>&1; then
		if python3 tools/audit-image.py "$image" >/tmp/audit.$$ 2>&1; then
			pass "the image audits (boot header, ramdisk, AVB digest)"
		else
			fail "the image audits (boot header, ramdisk, AVB digest)"
			tail -n 6 /tmp/audit.$$ | sed 's/^/    /'
		fi
		rm -f /tmp/audit.$$
	else
		skip "the image's structure" "no python3"
	fi
fi

# 4. The phone's own partitions: every path and module the configuration
#    names has to be in the tree it is taken from.
if [ -n "$firmware" ]; then
	# A partition that was unmounted leaves an empty directory behind, and
	# every path would read as missing: that is not what the check is for.
	empty=""
	for tree in vendor odm; do
		if [ ! -d "$firmware/$tree" ] || [ -z "$(ls -A "$firmware/$tree" 2>/dev/null | head -n 1)" ]; then
			empty="$empty $tree"
		fi
	done
	if [ -n "$empty" ]; then
		skip "the paths the configuration names" "$firmware/$empty is empty: is the partition still mounted?"
	elif "$packer" paths --config "$config" --vendor "$firmware/vendor" --odm "$firmware/odm" >/tmp/paths.$$ 2>&1; then
		pass "every path the configuration names is in the firmware"
	else
		fail "every path the configuration names is in the firmware"
		grep -E "MISSING|not checked" /tmp/paths.$$ | sed 's/^/    /' | head -n 12
	fi
	rm -f /tmp/paths.$$
else
	skip "the paths the configuration names" "give --firmware"
fi

echo
echo "$passed passed, $failed failed, $skipped skipped"
[ "$failed" -eq 0 ]
