#!/usr/bin/env bash
set -euo pipefail

# Run as root while the runner is idle: tune.sh <runner systemd unit>
runner=$1
bench=8-11
sys=/sys/devices/system/cpu
systemctl cat "$runner" >/dev/null

expand() {
  local part
  for part in ${1//,/ }; do seq "${part%-*}" "${part#*-}"; done
}

siblings=
for cpu in $(expand "$bench"); do
  for sibling in $(expand "$(cat "$sys/cpu$cpu/topology/thread_siblings_list")"); do
    [[ "$sibling" == "$cpu" ]] && continue
    if expand "$bench" | grep -qx "$sibling"; then
      echo "CPUs $cpu and $sibling share a core, pick four separate cores" >&2
      exit 1
    fi
    siblings+="$sibling "
  done
done

# Everything except the benchmark keeps to the remaining online CPUs.
rest=
{
  for cpu in $(expand "$(cat "$sys/present")"); do
    expand "$bench" | grep -qx "$cpu" && continue
    if [[ " $siblings" == *" $cpu "* || $(cat "$sys/cpu$cpu/online" 2>/dev/null) == 0 ]]; then
      echo "w $sys/cpu$cpu/online - - - - 0"
    else
      rest+="${rest:+,}$cpu"
    fi
  done
  for cpu in $(expand "$bench"); do
    echo "w $sys/cpu$cpu/cpufreq/scaling_governor - - - - performance"
  done
  echo "w $sys/intel_pstate/no_turbo - - - - 1"
  echo "w $sys/cpufreq/boost - - - - 0"
} >/etc/tmpfiles.d/fff-perf.conf
systemd-tmpfiles --create /etc/tmpfiles.d/fff-perf.conf

for unit in init.scope system.slice user.slice; do
  systemctl set-property "$unit" AllowedCPUs="$rest"
done
mkdir -p "/etc/systemd/system/$runner.d"
printf '[Service]\nSlice=fff-perf.slice\nCPUAffinity=%s\n' "$rest" >"/etc/systemd/system/$runner.d/fff-perf.conf"
systemctl daemon-reload
systemctl restart "$runner"
