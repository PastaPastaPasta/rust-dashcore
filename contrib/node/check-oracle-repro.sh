#!/usr/bin/env bash
# Prove that the Dash Core oracle archives are reproducible: build each pin from
# scratch twice, in two fresh cache roots with different paths (each builds its own
# depends tree; only the depends downloads are shared), and require both archives
# to be byte-identical to each other and to the sha256 recorded in pins.txt.
#
# Usage: contrib/node/check-oracle-repro.sh --work DIR [options] [pin...]   (default: every pin)
#   --work DIR           scratch directory for the two cache roots (must not exist or be empty)
#   --flavor plain|fuzz  as build-oracle.sh (default plain)
#   --sources DIR        depends downloads to share (default: $DASH_ORACLE_CACHE/depends-sources
#                        or /work/cache/oracle/depends-sources)
#   --jobs N             parallel jobs per build (default: nproc / 2; the two builds run at once)
#   --keep               keep the two cache roots (default: delete them when the check passes)
#   --update-pins        move each pin to the head of its branch first (DASH_SRC or --dash-src),
#                        and record the archive's sha256 in pins.txt if both roots agree
#   --dash-src DIR       Dash Core repository (default: $DASH_SRC or ~/workspace/dash)
#   --install DIR        after a successful check, copy each archive (with its MANIFEST) into
#                        the cache root DIR, e.g. /work/cache/oracle
#
# Exit status 0 only if every pin reproduced its pinned archive in both roots.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
build_oracle="${script_dir}/build-oracle.sh"
pins_file="${script_dir}/../../dash-oracle-sys/pins.txt"

work=""
flavor=plain
sources="${DASH_ORACLE_CACHE:-/work/cache/oracle}/depends-sources"
jobs=$(( $(nproc) / 2 ))
keep=0
update_pins=0
dash_src="${DASH_SRC:-${HOME}/workspace/dash}"
install=""
pins=()
while [ $# -gt 0 ]; do
    case "$1" in
        --work) work="$2"; shift 2 ;;
        --flavor) flavor="$2"; shift 2 ;;
        --sources) sources="$2"; shift 2 ;;
        --jobs) jobs="$2"; shift 2 ;;
        --keep) keep=1; shift ;;
        --update-pins) update_pins=1; shift ;;
        --dash-src) dash_src="$2"; shift 2 ;;
        --install) install="$2"; shift 2 ;;
        -h|--help) sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "unknown option $1" >&2; exit 2 ;;
        *) pins+=("$1"); shift ;;
    esac
done
[ -n "${work}" ] || { echo "--work is required" >&2; exit 2; }
if [ -e "${work}" ] && [ -n "$(ls -A "${work}")" ]; then
    echo "${work} is not empty" >&2; exit 2
fi
if [ ${#pins[@]} -eq 0 ]; then
    mapfile -t pins < <(awk '!/^#/ && NF { print $1 }' "${pins_file}")
fi
col=4; [ "${flavor}" = fuzz ] && col=5

# pins.txt column $3 of pin $1 := $2
set_pin_field() {
    awk -v p="$1" -v col="$3" -v v="$2" 'BEGIN { OFS = "  " } $1 == p { $col = v } { print }' \
        "${pins_file}" > "${pins_file}.new" && mv "${pins_file}.new" "${pins_file}"
}
if [ "${update_pins}" = 1 ]; then
    for pin in "${pins[@]}"; do
        branch="$(awk -v p="${pin}" '$1 == p { print $3 }' "${pins_file}")"
        commit="$(git -C "${dash_src}" rev-parse "${branch}^{commit}")"
        if [ "${commit}" != "$(awk -v p="${pin}" '$1 == p { print $2 }' "${pins_file}")" ]; then
            # a new commit: every recorded archive digest is stale
            set_pin_field "${pin}" "${commit}" 2
            set_pin_field "${pin}" - 4
            set_pin_field "${pin}" - 5
        else
            set_pin_field "${pin}" - "${col}"
        fi
        echo "${pin}: ${commit} (${branch})"
    done
fi

# Two roots whose paths differ in length as well as in name.
roots=("${work}/a" "${work}/second-root")
mkdir -p "${roots[@]}"
pids=()
for root in "${roots[@]}"; do
    "${build_oracle}" --force --flavor "${flavor}" --cache "${root}" --sources "${sources}" --jobs "${jobs}" \
        --dash-src "${dash_src}" "${pins[@]}" \
        > "${root}.log" 2>&1 &
    pids+=($!)
done
status=0
for pid in "${pids[@]}"; do wait "${pid}" || status=1; done

failed=0
for pin in "${pins[@]}"; do
    commit="$(awk -v p="${pin}" '$1 == p { print $2 }' "${pins_file}")"
    expected="$(awk -v p="${pin}" -v c="${col}" '$1 == p { print $c }' "${pins_file}")"
    shas=()
    for root in "${roots[@]}"; do
        # published if it matched pins.txt, otherwise kept under rejected/
        dir="$("${build_oracle}" --print-dir --flavor "${flavor}" --cache "${root}" "${pin}")"
        archive="$(ls "${dir}"/libdash_oracle_*.a "${root}/rejected/${commit}-${dir##*/}"-*.a 2>/dev/null | head -1 || true)"
        if [ -z "${archive}" ]; then
            echo "${pin}: no archive built in ${root} (see ${root}.log)"
            shas+=("none")
        else
            shas+=("$(sha256sum "${archive}" | cut -d' ' -f1)")
        fi
    done
    echo "${pin} ${flavor}: pins.txt ${expected}"
    echo "${pin} ${flavor}: ${roots[0]} ${shas[0]}"
    echo "${pin} ${flavor}: ${roots[1]} ${shas[1]}"
    if [ "${shas[0]}" = none ] || [ "${shas[0]}" != "${shas[1]}" ]; then
        echo "${pin} ${flavor}: NOT REPRODUCIBLE"
        failed=1
    elif [ "${update_pins}" = 1 ] && [ "${expected}" = "-" ]; then
        set_pin_field "${pin}" "${shas[0]}" "${col}"
        echo "${pin} ${flavor}: reproduced in both roots; recorded ${shas[0]} in pins.txt"
    elif [ "${expected}" != "${shas[0]}" ]; then
        echo "${pin} ${flavor}: reproducible, but not the archive pins.txt records"
        failed=1
    else
        echo "${pin} ${flavor}: reproduced the pinned archive in both roots"
    fi
done

if [ "${failed}" = 0 ] && [ "${status}" = 0 ]; then
    if [ -n "${install}" ]; then
        for pin in "${pins[@]}"; do
            commit="$(awk -v p="${pin}" '$1 == p { print $2 }' "${pins_file}")"
            dir="$("${build_oracle}" --print-dir --flavor "${flavor}" --cache "${roots[0]}" "${pin}")"
            src="${dir}" dest="${install}/${commit}/${dir##*/}"
            mkdir -p "${install}/${commit}"
            rm -rf "${dest}.new" && cp -r "${src}" "${dest}.new" && rm -rf "${dest}" && mv "${dest}.new" "${dest}"
            echo "${pin} ${flavor}: installed in ${dest}"
        done
    fi
    [ "${keep}" = 1 ] || rm -rf "${roots[@]}"
    exit 0
fi
echo "kept ${roots[*]} for inspection"
exit 1
