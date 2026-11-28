#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
generate=()
run_dir=
subnet=${DEVNET_SUBNET:-}
hold=
while (($#)); do
    case $1 in
        --) shift; break ;;
        --hold) hold=${2:?missing duration}; shift 2 ;;
        --subnet|--run-dir|--plan|--dashd|--rdashd|--devnet-name|--needed-hosts|--needed-host|--harness-tree|--high-subsidy-blocks|--high-subsidy-factor|--minimum-difficulty-blocks)
            [[ $1 != --subnet ]] || subnet=${2:?missing subnet}
            [[ $1 != --run-dir ]] || run_dir=${2:?missing directory}
            generate+=("$1" "${2:?missing value}")
            shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
[[ -n $run_dir && -n $subnet ]] || { echo "--run-dir and subnet are required" >&2; exit 2; }
[[ -z $hold || ( $hold =~ ^[0-9]+$ && $# == 0 ) ]] || { echo "invalid --hold" >&2; exit 2; }
life=(python3 -I -B "$here/lifecycle.py")
"${life[@]}" preflight --subnet "$subnet"
run_dir=$(realpath -m -- "$run_dir")
export O12B_RUN_ID
O12B_RUN_ID=$(tr -d - </proc/sys/kernel/random/uuid)
export DEVNET_RUN_DIR=$run_dir COMPOSE_PROJECT_NAME=o12b-devnet-$O12B_RUN_ID
child=
cleanup() {
    local status=$?
    trap - EXIT
    trap '' INT TERM
    if [[ -n $child ]]; then
        kill -TERM -- "-$child" 2>/dev/null || true
        wait "$child" 2>/dev/null || true
    fi
    local owner=
    if [[ -f $run_dir/state.json ]]; then
        owner=$(python3 -I -B -c 'import json, sys; print(json.load(sys.stdin)["run_id"])' < "$run_dir/state.json") || exit 1
    fi
    if [[ $owner == "$O12B_RUN_ID" ]]; then
        "$here/down.sh" --force --run-dir "$run_dir" || status=1
        "${life[@]}" verify --run-dir "$run_dir" || status=1
    fi
    exit "$status"
}
run_child() {
    setsid -- setpriv --pdeathsig TERM -- "$@" <&0 &
    child=$!
    wait "$child"
    child=
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
python3 -I -B "$here/gen.py" "${generate[@]}"
run_child "${life[@]}" build --run-dir "$run_dir"
echo compose-start > "$run_dir/phase"
run_child docker compose -p "$COMPOSE_PROJECT_NAME" -f "$run_dir/compose.yaml" up -d --pull never
run_child "${life[@]}" peering --run-dir "$run_dir"
if (($#)); then
    run_child "$@"
elif [[ -n $hold ]]; then
    run_child sleep "$hold"
fi
