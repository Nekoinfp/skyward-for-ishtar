#!/bin/sh
# Gather, from a computer with adb, what tells why a xuanyuan's recovery is not
# behaving: the phone in recovery, `adb devices` showing it, then
#
#   tools/collect-report.sh [report.txt]
#
# Everything here only reads. Each part stands alone, so one that the phone
# cannot answer still leaves the rest in the file.
set -u

out=${1:-"xuanyuan-report-$(date +%Y%m%d-%H%M%S).txt"}
: > "$out"

say() {
	printf '\n===== %s =====\n' "$1" >> "$out"
}

# Run a command on the phone and keep its output, saying so when it cannot be
# run rather than leaving a silent gap.
ask() {
	adb shell "$1" >> "$out" 2>&1 || printf '(that command failed on the phone)\n' >> "$out"
}

printf 'collected %s\n' "$(date)" >> "$out"
printf 'this report holds no key material: the key files are listed by name and size only\n' >> "$out"

say 'adb'
adb devices >> "$out" 2>&1

say 'skyward check'
ask 'skyward check'

say 'version'
ask 'skyward --version'

say 'slot'
ask 'skyward slot'

say 'kernel'
ask 'uname -a'
ask 'cat /proc/cmdline'

say 'an update in flight'
ask 'skyward update'

say 'logical partitions (first 200 lines)'
ask 'skyward lp | head -n 200'

say "the phone's own table"
ask 'cat /vendor/etc/fstab.qcom'
ask 'cat /odm/etc/fstab.qcom'

say 'mounts'
ask 'cat /proc/mounts'

say 'partitions by name'
ask 'ls -l /dev/block/by-name/'

say 'kernel modules'
ask "cut -d' ' -f1 /proc/modules"

say 'input devices'
ask 'cat /proc/bus/input/devices'

say 'properties'
ask 'getprop'

say 'what each service said'
ask 'for f in /skyward/run/services/*.log; do [ -f "$f" ] && { echo "--- $f"; tail -n 40 "$f"; }; done'

say 'recovery log (last 400 lines)'
ask 'tail -n 400 /skyward/run/recovery.log'

say 'kernel log (last 2000 lines)'
ask 'dmesg | tail -n 2000'

printf '\nwrote %s\n' "$out"
