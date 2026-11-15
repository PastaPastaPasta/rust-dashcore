#!/usr/bin/env bash
# Build the Dash Core consensus oracle (libdash_oracle) for dash-oracle-sys.
#
# For each pin in dash-oracle-sys/pins.txt this builds the pinned commit of the
# Dash Core branch that carries src/test/oracle (node/oracle-shim-<pin>) from a
# clean `git archive`, against Core's depends tree, and turns the result into one
# self-contained static archive whose only global symbols are the renamed entry
# points oracle_<pin>_* (pin "v24.0" -> prefix "oracle_v24_0_"). Everything else
# is localized, so several pins can be linked into one Rust binary.
#
# Needs Linux with GNU binutils (ld -r, objcopy). Outputs are cached under
# $DASH_ORACLE_CACHE/<commit>/<subdir>/ (default cache /work/cache/oracle) together with
# a MANIFEST recording the recipe inputs, the toolchain and sha256. <subdir> is the
# flavor's name, except fuzz-nocmp for fuzz (BL-371); a fuzz recipe change that moves
# its sha256 takes a new <subdir>, so trees still pinned to the old archive keep it.
#
# The archive's sha256 is its identity: a cached archive is reused only if the
# sha256 of its bytes equals pins.txt, and a fresh build whose sha256 differs
# from pins.txt is not published (the script fails). A pin whose sha256 column
# is "-" (not recorded yet) is built and published, but dash-oracle-sys refuses
# to link it unless DASH_ORACLE_ALLOW_UNPINNED=1.
#
# Usage: contrib/node/build-oracle.sh [options] [pin...]     (default: every pin)
#   --flavor plain|fuzz|ubsan
#                        plain: the system compiler, as release builds;
#                        fuzz: clang with -fsanitize=fuzzer-no-link (coverage for libFuzzer;
#                        relic and dashbls without trace-cmp, see oracle-relic/nocmp-clang);
#                        ubsan: gcc with -fsanitize=undefined (recoverable; see
#                        dash-oracle-sys/README.md) (--dev builds only; never pinned)
#   --dash-src DIR       Dash Core repository (default: $DASH_SRC or ~/workspace/dash)
#   --cache DIR          cache root (default: $DASH_ORACLE_CACHE or /work/cache/oracle)
#   --sources DIR        depends download directory (default: <cache>/depends-sources);
#                        its tarballs are checked against depends' own hashes
#   --jobs N             parallel jobs (default: nproc)
#   --force              rebuild even if the cache has the pinned archive
#   --update-pins        build the head of each pin's branch and record its commit
#                        and archive sha256 in pins.txt
#   --print-dir          only print the output directory of each pin
#   --dev DIR            development build of the checkout DIR (its working tree,
#                        uncommitted changes included) in a build directory that is
#                        kept between runs; publishes under <cache>/dev/<commit>/<subdir>
#                        (commit from pins.txt) and never touches the pinned cache.
#                        Link it with DASH_ORACLE_CACHE=<cache>/dev DASH_ORACLE_ALLOW_UNPINNED=1.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
pins_file="${repo_root}/dash-oracle-sys/pins.txt"
script_sha="$(sha256sum "${BASH_SOURCE[0]}" | cut -d' ' -f1)"
relic_tools="${script_dir}/oracle-relic"

flavor=plain
dash_src="${DASH_SRC:-${HOME}/workspace/dash}"
cache="${DASH_ORACLE_CACHE:-/work/cache/oracle}"
sources=""
jobs="$(nproc)"
force=0
update_pins=0
print_dir=0
dev_src=""
pins=()

while [ $# -gt 0 ]; do
    case "$1" in
        --flavor) flavor="$2"; shift 2 ;;
        --dash-src) dash_src="$2"; shift 2 ;;
        --cache) cache="$2"; shift 2 ;;
        --sources) sources="$2"; shift 2 ;;
        --jobs) jobs="$2"; shift 2 ;;
        --force) force=1; shift ;;
        --update-pins) update_pins=1; force=1; shift ;;
        --print-dir) print_dir=1; shift ;;
        --dev) dev_src="$(cd "$2" && pwd)"; shift 2 ;;
        -h|--help) sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "unknown option $1" >&2; exit 2 ;;
        *) pins+=("$1"); shift ;;
    esac
done
sources="${sources:-${cache}/depends-sources}"
if [ "${flavor}" = ubsan ] && [ -z "${dev_src}" ]; then
    echo "the ubsan flavor is for --dev builds (it is never pinned)" >&2; exit 2
fi
if [ -n "${dev_src}" ] && [ "${update_pins}" = 1 ]; then
    echo "--dev and --update-pins exclude each other" >&2; exit 2
fi

# Per flavor: compilers and flags, the pins.txt sha256 column, the cache directory, and
# how the link check is linked and run (the fuzz flavor needs libFuzzer for its coverage callbacks).
case "${flavor}" in
    # -fno-gnu-unique: gcc's STB_GNU_UNIQUE symbols cannot be localized (see below)
    plain)
        cc=gcc; cxx=g++; extra_flags="-fno-gnu-unique"; sha_col=4; cache_subdir=plain
        check_ldflags=(); check_args=()
        check_main='int main(void) { return ABI() > 0 ? 0 : 1; }' ;;
    ubsan)
        # Recoverable checks, as Core's CI runs them: the tests abort on the first report
        # through UBSAN_OPTIONS=halt_on_error=1 with dash-oracle-sys/ubsan-suppressions.txt.
        cc=gcc; cxx=g++; extra_flags="-fno-gnu-unique -fsanitize=undefined"; sha_col=""; cache_subdir=ubsan  # never pinned
        check_ldflags=(-fsanitize=undefined); check_args=()
        check_main='int main(void) { return ABI() > 0 ? 0 : 1; }' ;;
    fuzz)
        cc="${relic_tools}/nocmp-clang"; cxx="${relic_tools}/nocmp-clang++"
        extra_flags="-fsanitize=fuzzer-no-link"; sha_col=5; cache_subdir=fuzz-nocmp
        check_ldflags=(-fsanitize=fuzzer); check_args=(-runs=1)
        check_main='int LLVMFuzzerTestOneInput(const unsigned char* d, unsigned long n) { (void)d; (void)n; return ABI() > 0 ? 0 : -1; }' ;;
    *) echo "unknown flavor ${flavor}" >&2; exit 2 ;;
esac

# The output must not depend on where, when or by whom it was built.
# - ccache: a hit returns an object compiled elsewhere, and CCACHE_BASEDIR rewrites
#   include paths to relative ones that -ffile-prefix-map then misses (both leak into
#   __FILE__ strings). It is disabled and its environment cleared.
# - Locale and time zone are fixed; SOURCE_DATE_EPOCH (below) pins __DATE__/__TIME__.
while IFS= read -r var; do unset "${var}"; done < <(env | sed -n 's/^\(CCACHE_[A-Z_]*\)=.*/\1/p')
export CCACHE_DISABLE=1 LC_ALL=C TZ=UTC
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS MAKEFLAGS

# pins.txt columns: pin commit branch sha256(plain) sha256(fuzz)
pin_field() { awk -v p="$1" -v f="$2" '$1 == p { print $f }' "${pins_file}"; }
if [ ${#pins[@]} -eq 0 ]; then
    mapfile -t pins < <(awk '!/^#/ && NF { print $1 }' "${pins_file}")
fi

log() { echo "[build-oracle] $*" >&2; }
manifest_field() { awk -v k="$2" '$1 == k { sub(/^[^ ]+ /, ""); print; exit }' "$1"; }

# Whether out/ holds the archive the pin records: the sha256 of the bytes on disk
# (not the MANIFEST's claim) and the MANIFEST's identity fields.
cached_archive_ok() {
    local out="$1" archive="$2" pin="$3" commit="$4" sym_prefix="$5" expected="$6"
    [ -f "${archive}" ] && [ -f "${out}/MANIFEST" ] || return 1
    local field want
    for field in "pin ${pin}" "commit ${commit}" "flavor ${flavor}" "symbol_prefix ${sym_prefix}"; do
        want="${field#* }"
        if [ "$(manifest_field "${out}/MANIFEST" "${field%% *}")" != "${want}" ]; then
            log "${pin}: cached MANIFEST does not say ${field}"
            return 1
        fi
    done
    local actual
    actual="$(sha256sum "${archive}" | cut -d' ' -f1)"
    if [ "${expected}" = "-" ]; then
        log "${pin}: cached ${archive} sha256 ${actual} (pins.txt records none: unpinned)"
        return 0
    fi
    if [ "${actual}" != "${expected}" ]; then
        log "${pin}: cached archive sha256 ${actual} is not the pinned ${expected}"
        return 1
    fi
    log "${pin}: cached ${archive} (sha256 verified)"
}

# Copy the working tree of checkout $1 (tracked and untracked, not ignored files) into $2.
sync_worktree() {
    mkdir -p "$2"
    (cd "$1" && { git ls-files -z; git ls-files -z -o --exclude-standard; }) |
        rsync -a --from0 --files-from=- "$1/" "$2/"
}

build_pin() {
    local pin="$1"
    local pin_id="${pin//./_}"
    local branch commit sym_prefix out build host
    branch="$(pin_field "${pin}" 3)"
    [ -n "${branch}" ] || { log "pin ${pin} is not in ${pins_file}"; return 1; }
    if [ "${update_pins}" = 1 ]; then
        commit="$(git -C "${dash_src}" rev-parse "${branch}^{commit}")"
    else
        commit="$(pin_field "${pin}" 2)"
    fi
    sym_prefix="oracle_${pin_id}_"
    local out_root="${cache}"
    [ -z "${dev_src}" ] || out_root="${cache}/dev"
    out="${out_root}/${commit}/${cache_subdir}"
    if [ "${print_dir}" = 1 ]; then echo "${out}"; return 0; fi

    # One build per commit and flavor at a time (cargo may run several build scripts).
    mkdir -p "${cache}/locks"
    exec {lock_fd}>"${cache}/locks/${commit}-${cache_subdir}${dev_src:+-dev}.lock"
    flock "${lock_fd}"

    local archive="${out}/libdash_oracle_${pin_id}.a" expected
    expected="-"
    [ -n "${dev_src}" ] || expected="$(pin_field "${pin}" "${sha_col}")"
    if [ "${force}" = 0 ] && [ -z "${dev_src}" ] &&
        cached_archive_ok "${out}" "${archive}" "${pin}" "${commit}" "${sym_prefix}" "${expected}"; then
        return 0
    fi

    local epoch source_desc
    if [ -n "${dev_src}" ]; then
        build="${cache}/dev/build/${pin}-${cache_subdir}"
        log "${pin}: dev build of ${dev_src} in ${build}"
        sync_worktree "${dev_src}" "${build}"
        epoch="$(git -C "${dev_src}" log -1 --format=%ct HEAD)"
        source_desc="dev $(git -C "${dev_src}" rev-parse HEAD) ${dev_src}"
    else
        git -C "${dash_src}" cat-file -e "${commit}^{commit}" || { log "${pin}: commit ${commit} not in ${dash_src}"; return 1; }
        build="${cache}/build/${commit}-${cache_subdir}"
        log "${pin}: building ${commit} (${branch}, ${flavor}) in ${build}"
        rm -rf "${build}"
        mkdir -p "${build}"
        git -C "${dash_src}" archive --format=tar "${commit}" | tar -x -C "${build}"
        epoch="$(git -C "${dash_src}" log -1 --format=%ct "${commit}")"
        source_desc="git ${commit}"
    fi
    # Each pin's depends tree gets its own cache: depends wipes a package's cache
    # directory when it stores a build, so pins with different recipes sharing one
    # BASE_CACHE would keep evicting each other. The key is the depends/ tree's hash.
    local depends_id
    if [ -n "${dev_src}" ]; then
        if [ -z "$(git -C "${dev_src}" status --porcelain -- depends)" ]; then
            depends_id="$(git -C "${dev_src}" rev-parse HEAD:depends)"
        else
            depends_id="dev-${pin}"
        fi
    else
        depends_id="$(git -C "${dash_src}" rev-parse "${commit}:depends")"
    fi
    local depends_cache="${cache}/depends-cache/${depends_id}"
    mkdir -p "${cache}/logs" "${sources}" "${depends_cache}"
    host="$("${build}/depends/config.guess")"
    local logf="${cache}/logs/${commit}-${cache_subdir}${dev_src:+-dev}.log"
    : > "${logf}"

    # Paths reach the compiler as absolute paths under ${build}, which -ffile-prefix-map
    # rewrites to "." (the depends prefix, ${build}/depends/<host>, becomes ./depends/<host>).
    # DASH_ORACLE_HOOKS compiles the oracle's seams into Core (the RNG draw log, the
    # AddrMan/TimeData observer, the peer snapshot, the inline validation queue); a
    # build without it has none of them. Only this tree defines it, so the dashd
    # published beside the archive carries them too, never installed.
    local flags="${extra_flags} -DDASH_ORACLE_HOOKS -ffile-prefix-map=${build}=."
    # set -e does not apply inside a subshell on the left of ||, hence the && chain.
    (
        cd "${build}" &&
        export SOURCE_DATE_EPOCH="${epoch}" &&
        # Only the libraries dashd needs; depends pins every version. Builds sharing a
        # depends cache, or a download directory, take turns here.
        flock "${sources}/.download.lock" flock "${cache}/locks/depends-${depends_id}.lock" \
            make -C depends -j"${jobs}" HOST="${host}" NO_QT=1 NO_WALLET=1 NO_ZMQ=1 NO_UPNP=1 NO_NATPMP=1 NO_USDT=1 \
            SOURCES_PATH="${sources}" BASE_CACHE="${depends_cache}" &&
        if [ ! -f config.status ]; then
            ./autogen.sh &&
            CONFIG_SITE="${build}/depends/${host}/share/config.site" CC="${cc}" CXX="${cxx}" \
                CFLAGS="${flags}" CXXFLAGS="${flags}" \
                ./configure --prefix=/ --disable-ccache --disable-wallet --without-gui --disable-zmq \
                --without-miniupnpc --without-natpmp --disable-bench --disable-tests --disable-fuzz-binary --disable-man
        fi &&
        # dashbls first: its make can regenerate relic_conf.h (rm, then mv), and Core's
        # objects that include dashbls's headers would otherwise compile in parallel with it.
        make -C src -j"${jobs}" dashbls/libdashbls.la &&
        make -C src -j"${jobs}" dashd libdash_oracle.a
    ) >> "${logf}" 2>&1 || { log "${pin}: build failed, see ${logf}"; tail -20 "${logf}" >&2; return 1; }

    local tmp="${build}/oracle-link" obj="${build}/oracle-link/oracle_${pin_id}.o"
    rm -rf "${tmp}"
    mkdir -p "${tmp}"
    mapfile -t entry_points < <(nm -g --defined-only "${build}/src/libdash_oracle.a" | awk '$3 ~ /^oracle_/ { print $3 }' | sort -u)
    [ ${#entry_points[@]} -gt 0 ] || { log "${pin}: no oracle_* symbols in libdash_oracle.a"; return 1; }
    # Every archive dashd links (unused ones contribute nothing: only members that
    # resolve a reference are pulled in).
    mapfile -t archives < <(cd "${build}" && find src depends/"${host}"/lib -name '*.a' -not -path '*/oracle-link/*' | sort | sed "s|^|${build}/|")

    # The shim replaces a Core function by referencing __real_<symbol> and defining
    # __wrap_<symbol> (oracle.cpp's quorum-member seam): --wrap then sends the calls
    # other Core objects make to the wrapper, and __real_<symbol> to Core's function.
    # The shim's seams: every __wrap_<symbol> it defines must come with a __real_<symbol>
    # reference and the other way round, or --wrap would be missing or dangling.
    local wraps=() wrapped=() shim_undefined shim_defined real_refs wrap_defs
    shim_undefined="$(nm -u "${build}/src/libdash_oracle.a")"
    shim_defined="$(nm -g --defined-only "${build}/src/libdash_oracle.a")"
    real_refs="$(awk '$2 ~ /^__real_/ { print substr($2, 8) }' <<<"${shim_undefined}" | sort -u)"
    wrap_defs="$(awk '$3 ~ /^__wrap_/ { print substr($3, 8) }' <<<"${shim_defined}" | sort -u)"
    if [ "${real_refs}" != "${wrap_defs}" ]; then
        log "${pin}: __real_ references and __wrap_ definitions differ:"
        diff <(echo "${real_refs}") <(echo "${wrap_defs}") >&2 || true
        return 1
    fi
    [ -z "${real_refs}" ] || mapfile -t wrapped <<<"${real_refs}"
    for sym in "${wrapped[@]}"; do wraps+=("--wrap=${sym}"); done

    # The relic call log (BS11a design §3.3, D59, D60), when the shim defines its receiver
    # dash_oracle_relic_note. The wrap set is every symbol that is undefined in a dashbls
    # object or in Core's bls object and a text symbol of a relic member, minus core_get
    # (the log's own accessor; it sets no code). wrap.py writes a __wrap_ for each from
    # relic.h's prototypes, and fails on any it cannot parse; throwsites.py checks every
    # relic error site the wrap set reaches against its class table and audited indirect
    # calls, reading the sources as the flavor's own compiler preprocesses them.
    local relic_inputs=() relic_set_sha="-" relic_count=0
    local rw="${tmp}/relic-wrap" bls_libs="${build}/src/dashbls/.libs" relic_src="${build}/src/dashbls/depends/relic"
    if awk '$3 == "dash_oracle_relic_note" { found = 1 } END { exit !found }' <<<"${shim_defined}"; then
        mkdir -p "${rw}"
        local callers=(libbitcoin_consensus_a-bls.o)
        mapfile -t -O 1 callers < <(ar t "${bls_libs}/libdashbls.a" | grep -x 'libdashbls_la-.*\.o')
        # nm -A prints "archive:member: U symbol"; keep the symbols of the caller members.
        nm -A -u "${bls_libs}/libdashbls.a" "${build}/src/libbitcoin_consensus.a" |
            awk -F: -v list="${callers[*]}" '
                BEGIN { n = split(list, c, " "); for (i = 1; i <= n; i++) want[c[i]] = 1 }
                $2 in want { seen[$2] = 1; k = split($3, f, " "); print f[k] }
                END { for (m in want) if (!(m in seen)) { print "missing " m > "/dev/stderr"; bad = 1 }; exit bad }' |
            sort -u > "${rw}/callers.U" ||
            { log "${pin}: a relic caller object has no undefined symbols"; return 1; }
        nm -g --defined-only "${bls_libs}/librelic.a" | awk '$2 == "T" { print $3 }' | sort -u > "${rw}/relic.T"
        comm -12 "${rw}/callers.U" "${rw}/relic.T" | grep -v -x core_get > "${rw}/wrapset.txt" || true
        relic_count="$(wc -l < "${rw}/wrapset.txt")"
        [ "${relic_count}" -gt 0 ] || { log "${pin}: the relic wrap set is empty"; return 1; }
        local both
        both="$(comm -12 "${rw}/wrapset.txt" <(printf '%s\n' "${wrapped[@]}" | sort -u))"
        [ -z "${both}" ] || { log "${pin}: the shim and the relic log both wrap:"; echo "${both}" >&2; return 1; }
        # Any other object linked here may call relic directly too (Core's objects do, through
        # dashbls's inline headers): every relic function it calls must be in the wrap set,
        # or that call would go unlogged. relic's own members and the shim are exempt.
        local outside
        outside="$(for a in "${archives[@]}"; do
                [ "${a}" = "${bls_libs}/librelic.a" ] || [ "${a}" = "${build}/src/libdash_oracle.a" ] || nm -A -u "${a}"
            done | awk -F: '$2 !~ /^librelic_la-/ { k = split($3, f, " "); print f[k] }' | sort -u |
            comm -12 - "${rw}/relic.T" | comm -23 - "${rw}/wrapset.txt" | grep -v -x core_get)" || true
        [ -z "${outside}" ] || { log "${pin}: relic functions called outside the wrap set:"; echo "${outside}" >&2; return 1; }
        relic_set_sha="$(sha256sum "${rw}/wrapset.txt" | cut -d' ' -f1)"
        {
            python3 "${relic_tools}/wrap.py" --cc "${cc}" --include "${relic_src}/include" \
                --include "${relic_src}/include/low" --symbols "${rw}/wrapset.txt" --out "${rw}/relic_wrap.c" &&
            "${cc}" -O2 -fPIC ${flags//-fno-gnu-unique/} -I "${relic_src}/include" -I "${relic_src}/include/low" \
                -c "${rw}/relic_wrap.c" -o "${rw}/relic_wrap.o" &&
            python3 "${relic_tools}/throwsites.py" --cc "${cc}" --include "${build}/depends/${host}/include" \
                --relic-archive "${bls_libs}/librelic.a" \
                --relic-src "${relic_src}" --dashbls-src "${build}/src/dashbls/src" --wrap-set "${rw}/wrapset.txt" \
                --classes "${relic_tools}/throwsites.tsv" --indirect "${relic_tools}/indirect.tsv" --report "${rw}/relic-throwsites.tsv"
        } >> "${logf}" 2>&1 || { log "${pin}: relic call log failed, see ${logf}"; tail -20 "${logf}" >&2; return 1; }
        local drift
        drift="$(diff "${rw}/wrapset.txt" <(nm -g --defined-only "${rw}/relic_wrap.o" |
            awk '$3 ~ /^__wrap_/ { print substr($3, 8) }' | sort))" ||
            { log "${pin}: the relic wrappers differ from the wrap set:"; echo "${drift}" >&2; return 1; }
        while IFS= read -r sym; do wraps+=("--wrap=${sym}"); done < "${rw}/wrapset.txt"
        relic_inputs=("${rw}/relic_wrap.o")
        log "${pin}: relic call log over ${relic_count} wrapped symbols"
    fi
    # One relocatable object holding the shim and everything it reaches. Section
    # groups are dissolved so that no COMDAT is shared with another pin's object.
    local undefined=()
    for sym in "${entry_points[@]}"; do undefined+=(-u "${sym}"); done
    ld -r --force-group-allocation "${wraps[@]}" -o "${tmp}/oracle.o" "${undefined[@]}" \
        "${relic_inputs[@]}" --start-group "${archives[@]}" --end-group
    local linked_undefined unwrapped
    linked_undefined="$(nm -u "${tmp}/oracle.o")"
    unwrapped="$(awk '$2 ~ /^__(real|wrap)_/ { print $2 }' <<<"${linked_undefined}")"
    [ -z "${unwrapped}" ] || { log "${pin}: wrapped symbols left unresolved:"; echo "${unwrapped}" >&2; return 1; }
    : > "${tmp}/rename.map"
    for sym in "${entry_points[@]}"; do
        echo "${sym} ${sym_prefix}${sym#oracle_}" >> "${tmp}/rename.map"
    done
    objcopy --redefine-syms="${tmp}/rename.map" "${tmp}/oracle.o" "${tmp}/renamed.o"
    # Core always builds with -g1; the debug info is ~95% of the object and holds paths.
    objcopy --wildcard --keep-global-symbol="${sym_prefix}*" --strip-debug "${tmp}/renamed.o" "${obj}"
    local leaked
    leaked="$(nm -g --defined-only "${obj}" | awk -v p="${sym_prefix}" 'index($3, p) != 1 { print $2, $3 }' | head -5 || true)"
    [ -z "${leaked}" ] || { log "${pin}: symbols other than ${sym_prefix}* stay global:"; echo "${leaked}" >&2; return 1; }
    # Nothing in the object may name where it was built. The ubsan flavor is exempt: its
    # diagnostics carry source paths the prefix maps do not rewrite, and it is never pinned.
    local dir
    for dir in "${build}" "${cache}" "${HOME}"; do
        if [ "${flavor}" != ubsan ] && grep -q -a -F "${dir}" "${obj}"; then
            log "${pin}: the object contains the path ${dir} (not reproducible)"; return 1
        fi
    done

    # The object must link with nothing but the C++ runtime (and libFuzzer for the fuzz flavor).
    printf 'int %sabi_version(void);\n#define ABI %sabi_version\n%s\n' "${sym_prefix}" "${sym_prefix}" "${check_main}" \
        > "${tmp}/link_check.c"
    "${cc}" "${check_ldflags[@]}" -o "${tmp}/link_check" "${tmp}/link_check.c" "${obj}" -lstdc++ -lm -lpthread \
        || { log "${pin}: oracle object does not link stand-alone"; return 1; }
    "${tmp}/link_check" "${check_args[@]}" >/dev/null 2>&1 || { log "${pin}: link check binary failed"; return 1; }

    ar rcsD "${tmp}/archive.a" "${obj}"
    local sha
    sha="$(sha256sum "${tmp}/archive.a" | cut -d' ' -f1)"
    if [ "${update_pins}" = 0 ] && [ "${expected}" != "-" ] && [ "${expected}" != "${sha}" ]; then
        local rejected="${cache}/rejected/${commit}-${cache_subdir}-${sha}.a"
        mkdir -p "${cache}/rejected"
        mv "${tmp}/archive.a" "${rejected}"
        log "${pin}: ERROR: built sha256 ${sha} is not the pinned ${expected}; not published (kept as ${rejected})."
        log "${pin}: a different toolchain or recipe, or a non-reproducible build; see dash-oracle-sys/README.md"
        return 1
    fi

    # Publish atomically: readers check for the archive, so it is renamed into place last.
    mkdir -p "${out}"
    rm -f "${archive}"
    cp "${build}/src/test/oracle/oracle.h" "${out}/oracle.h"
    strip -o "${out}/dashd" "${build}/src/dashd"
    {
        echo "pin ${pin}"
        echo "branch ${branch}"
        echo "commit ${commit}"
        echo "source ${source_desc}"
        echo "flavor ${flavor}"
        echo "host ${host}"
        echo "symbol_prefix ${sym_prefix}"
        echo "recipe build-oracle.sh sha256 ${script_sha}"
        echo "flags ${flags//${build}/<build>}"
        echo "source_date_epoch ${epoch}"
        echo "depends $(cd "${build}/depends/${host}" && ls -a | sed -n 's/^\.stamp_//p' | tr '\n' ' ')"
        echo "cc $("${cc}" --version | head -1)"
        echo "cxx $("${cxx}" --version | head -1)"
        echo "ld $(ld --version | head -1)"
        echo "objcopy $(objcopy --version | head -1)"
        echo "entry_points ${#entry_points[@]}"
        echo "wrapped ${wrapped[*]:--}"
        echo "relic_wrapped ${relic_count} wrapset_sha256 ${relic_set_sha}"
        echo "relic_tools $(cd "${relic_tools}" && sha256sum wrap.py throwsites.py throwsites.tsv indirect.tsv nocmp-clang | awk '{ printf "%s %s ", $2, $1 }')"
        echo "depends_cache ${depends_id}"
        echo "sha256 ${sha}"
    } > "${tmp}/MANIFEST"
    mv "${tmp}/MANIFEST" "${out}/MANIFEST"
    if [ "${relic_count}" -gt 0 ]; then
        cp "${rw}/wrapset.txt" "${out}/relic-wrapset.txt"
        cp "${rw}/relic-throwsites.tsv" "${out}/relic-throwsites.tsv"
    fi
    mv "${tmp}/archive.a" "${archive}"
    log "${pin}: ${archive} sha256 ${sha}"

    if [ "${update_pins}" = 1 ]; then
        awk -v p="${pin}" -v c="${commit}" -v col="${sha_col}" -v s="${sha}" \
            'BEGIN { OFS = "  " } $1 == p { $2 = c; $col = s } { print }' "${pins_file}" > "${pins_file}.new"
        mv "${pins_file}.new" "${pins_file}"
        log "${pin}: recorded ${commit} ${sha} in pins.txt"
    elif [ "${expected}" = "-" ]; then
        log "${pin}: pins.txt records no ${flavor} sha256: this archive is unpinned"
    fi
    [ -n "${dev_src}" ] || rm -rf "${build}"
}

for pin in "${pins[@]}"; do
    # a subshell, so the per-pin lock is released when it returns
    ( build_pin "${pin}" )
done
