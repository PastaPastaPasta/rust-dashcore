#!/usr/bin/env bash
# Reproduce the CS2 floating-point gate evidence (designs/CS2-fp-release-binaries.md).
#
#   reproduce.sh WORKDIR
#
# Downloads the official dashd v23.1.8 and v24.0.0-rc.3 release archives for all six
# shipped platforms (x86_64, aarch64 and riscv64 Linux; win64; x86_64 and arm64 macOS),
# verifies the release SHA256SUMS.asc signatures against the pinned keys in
# ../keys/dash-release-keys.asc (the primary fingerprints listed in Dash Core's
# contrib/verify-commits/trusted-keys) and the archives against SHA256SUMS, locates the
# floating-point consensus code in each dashd, checks the findings (check_listings.py),
# measures KGW's `pow` table under every libm reachable here, asserts which variant each
# run used, and runs the KGW proof (kgw_proof.py) against the fixtures in ../out/pow-tables.
#
# Needs: gh, gpg, gcc, aarch64-linux-gnu-gcc and riscv64-linux-gnu-gcc, qemu-user with
# /usr/{aarch64,riscv64}-linux-gnu (libc6-arm64-cross, libc6-riscv64-cross), unzip, python3;
# docker is optional (glibc 2.31 and 2.36 runs). Downloads go to WORKDIR/dl (untrusted data);
# scripts run from this directory and only read the binaries, except the win64 `pow` routine,
# which tools/winpow.c maps and executes natively (Linux x86_64 only).
set -euo pipefail

tools="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
evidence="$(cd "${tools}/.." && pwd)"
work="${1:?usage: reproduce.sh WORKDIR}"
mkdir -p "${work}"/{dl,bin,report,out,gnupg}
chmod 700 "${work}/gnupg"
cd "${work}"
export GNUPGHOME="${work}/gnupg"

pins=(v23.1.8 v24.0.0-rc.3)
# Dash Core contrib/verify-commits/trusted-keys at both pins.
trusted=(29590362EC878A81FD3C202B52527BEDABE87984 3F5D48C9F00293CD365A3A9883592BD1400D58D9 15191D05B5CF956FE37C95962176C4A5D01EA524)
platforms=(x86_64 aarch64 riscv64 win64 x86_64-darwin arm64-darwin)
archive() { # VERSION PLATFORM
    case "$2" in
        x86_64 | aarch64 | riscv64) echo "dashcore-$1-$2-linux-gnu.tar.gz" ;;
        win64) echo "dashcore-$1-win64.zip" ;;
        *-darwin) echo "dashcore-$1-${2%-darwin}-apple-darwin.tar.gz" ;;
    esac
}
dashd() { [[ "$2" == win64 ]] && echo "bin/$1-$2/dashd.exe" || echo "bin/$1-$2/dashd"; }

echo "== download and verify"
gpg --quiet --import "${evidence}/keys/dash-release-keys.asc"
for t in "${pins[@]}"; do
    v="${t#v}"
    mkdir -p "dl/${t}"
    [[ -f "dl/${t}/SHA256SUMS.asc" ]] || gh release download "${t}" -R dashpay/dash -p SHA256SUMS.asc -D "dl/${t}"
    # Every signature must be good and from a trusted primary key; an unknown signer fails.
    status="$(gpg --status-fd 1 --verify "dl/${t}/SHA256SUMS.asc" 2> /dev/null || true)"
    good="$(grep -c '^\[GNUPG:\] VALIDSIG' <<< "${status}" || true)"
    sigs="$(grep -cE '^\[GNUPG:\] (GOODSIG|BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG)' <<< "${status}" || true)"
    [[ "${good}" -ge 1 && "${good}" == "${sigs}" ]] || { echo "${t}: SHA256SUMS.asc signature check failed"; exit 1; }
    for fpr in $(awk '/^\[GNUPG:\] VALIDSIG/ {print $NF}' <<< "${status}"); do
        printf '%s\n' "${trusted[@]}" | grep -qx "${fpr}" || { echo "${t}: signer ${fpr} not in trusted-keys"; exit 1; }
    done
    echo "${t}: ${good} good signature(s) from trusted keys"
    for p in "${platforms[@]}"; do
        f="$(archive "${v}" "${p}")"
        [[ -f "dl/${t}/${f}" ]] || gh release download "${t}" -R dashpay/dash -p "${f}" -D "dl/${t}"
        (cd "dl/${t}" && grep -E " ${f//./\\.}$" SHA256SUMS.asc | sha256sum -c -)
        mkdir -p "bin/${v}-${p}"
        if [[ "${p}" == win64 ]]; then
            unzip -o -q -j "dl/${t}/${f}" "dashcore-${v}/bin/dashd.exe" -d "bin/${v}-${p}"
        else
            tar -xzf "dl/${t}/${f}" -C "bin/${v}-${p}" --strip-components=2 "dashcore-${v}/bin/dashd"
        fi
    done
done

echo "== python venv"
if [[ ! -x venv/bin/python ]]; then
    python3 -m venv venv
    venv/bin/python -m pip -q install capstone==5.0.7 pyelftools==0.32 gmpy2==2.3.2 lief==1.0.0
fi
py() { venv/bin/python -I -B "$@"; }

echo "== how each dashd reaches pow"
for v in 23.1.8 24.0.0-rc.3; do
    for a in x86_64 aarch64 riscv64; do
        printf '%s-%s: ' "${v}" "${a}"
        readelf --dyn-syms -W "bin/${v}-${a}/dashd" | grep -oE ' pow@GLIBC_[0-9.]+' | sort -u | tr '\n' ' '
        echo
    done
    printf '%s-win64: math imports from msvcrt.dll: ' "${v}"
    x86_64-w64-mingw32-objdump -p "bin/${v}-win64/dashd.exe" 2> /dev/null | grep -cwE '(pow|exp|log|log2|exp2)$' || true
done

echo "== index functions and resolve data references"
for v in 23.1.8 24.0.0-rc.3; do
    for p in "${platforms[@]}"; do
        py "${tools}/xref.py" "$(dashd "${v}" "${p}")" index &
    done
done
wait

echo "== locate and dump the three FP sites"
fn_of() { # BIN QUERY... -> first function start address
    py "${tools}/xref.py" "$@" | grep -oE 'fn 0x[0-9a-f]+' | head -1 | cut -d' ' -f2
}
largest() { # BIN QUERY... -> start of the largest matching function (x86_64 also has .cold parts)
    local best=0 out="" lo hi
    for r in $(py "${tools}/xref.py" "$@" | grep -oE 'fn 0x[0-9a-f]+-0x[0-9a-f]+' | cut -d' ' -f2); do
        lo="${r%-*}"; hi="${r#*-}"
        if (( hi - lo > best )); then best=$(( hi - lo )); out="${lo}"; fi
    done
    echo "${out}"
}
for v in 23.1.8 24.0.0-rc.3; do
    pin="v${v%%.*}"
    for p in "${platforms[@]}"; do
        b="$(dashd "${v}" "${p}")"
        if [[ "${p}" == *-darwin ]]; then  # macOS binaries keep their symbols
            sub="$(py "${tools}/xref.py" "${b}" sym ZL21GetBlockSubsidyHelper | cut -d' ' -f1)"
            kgw="$(py "${tools}/xref.py" "${b}" sym Z19GetNextWorkRequired | cut -d' ' -f1)"
            ccbh="$(py "${tools}/xref.py" "${b}" sym ZL26ContextualCheckBlockHeader | cut -d' ' -f1)"
        else
            sub="$(fn_of "${b}" str isDevnet)"
            kgw="$(fn_of "${b}" dbl -1.228)"
            ccbh="$(largest "${b}" str 'DGW pre-fork')"
        fi
        py "${tools}/xref.py" "${b}" func "${sub}" > "report/${pin}-${p}-subsidy.s"
        py "${tools}/xref.py" "${b}" func "${kgw}" > "report/${pin}-${p}-kgw.s"
        py "${tools}/xref.py" "${b}" func "${ccbh}" > "report/${pin}-${p}-ccbh.s"
    done
done

echo "== check the findings"
py "${tools}/check_listings.py" report out/excerpts | tee out/check-listings.txt

echo "== KGW: pow over every reachable PastBlocksMass, per libm, with the variant asserted"
fix="${evidence}/out/pow-tables"
same() { cmp -s <(cut -d' ' -f1,3 "$1") <(cut -d' ' -f1,3 "$2"); }
same_col() { cmp -s <(awk '{print $1, $NF}' "$1") <(cut -d' ' -f1,3 "$2"); }
expect() { # MEASURED FIXTURE LABEL
    if same "$1" "$2"; then echo "PASS $3 = $(basename "$2")"; else echo "FAIL $3 != $(basename "$2")"; exit 1; fi
}
gcc -O2 -ffp-contract=off -o out/kgw_ehd_x86 "${tools}/kgw_ehd.c" -lm
aarch64-linux-gnu-gcc -O2 -ffp-contract=off -o out/kgw_ehd_a64 "${tools}/kgw_ehd.c" -lm
riscv64-linux-gnu-gcc -O2 -ffp-contract=off -o out/kgw_ehd_rv64 "${tools}/kgw_ehd.c" -lm
ldd --version | head -1 | tee out/host-glibc.txt
# glibc picks the FMA ifunc iff FMA and AVX2 are usable; check what the tunable actually masked.
features() { # [TUNABLE] -> "FMA AVX2" usable bits (0/1) as ld.so sees them
    env ${1:+GLIBC_TUNABLES=$1} /lib64/ld-linux-x86-64.so.2 --list-diagnostics | python3 -I -c '
import sys
d = dict(l.strip().split("=", 1) for l in sys.stdin if "=" in l)
ecx = int(d["x86.cpu_features.features[0x0].active[0x2]"], 16)  # CPUID.1:ECX, FMA = bit 12
ebx = int(d["x86.cpu_features.features[0x1].active[0x1]"], 16)  # CPUID.7:EBX, AVX2 = bit 5
print(ecx >> 12 & 1, ebx >> 5 & 1)'
}
read -r fma avx2 <<< "$(features)"
out/kgw_ehd_x86 > out/ehd-x86_64-default.txt
if [[ "${fma}${avx2}" == 11 ]]; then
    expect out/ehd-x86_64-default.txt "${fix}/glibc-fma.txt" "host glibc, default ifunc (FMA+AVX2 usable)"
    read -r f2 a2 <<< "$(features glibc.cpu.hwcaps=-AVX2,-FMA)"
    [[ "${f2}${a2}" == 00 ]] || { echo "FAIL tunable did not mask FMA/AVX2"; exit 1; }
    GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA out/kgw_ehd_x86 > out/ehd-x86_64-sse2.txt
    expect out/ehd-x86_64-sse2.txt "${fix}/glibc-sse2.txt" "host glibc, FMA/AVX2 masked (SSE2 ifunc)"
else
    expect out/ehd-x86_64-default.txt "${fix}/glibc-sse2.txt" "host glibc, no FMA+AVX2 (SSE2 ifunc)"
fi
qemu-aarch64 -L /usr/aarch64-linux-gnu out/kgw_ehd_a64 > out/ehd-aarch64.txt
expect out/ehd-aarch64.txt "${fix}/glibc-fma.txt" "glibc aarch64 (qemu)"
qemu-riscv64 -L /usr/riscv64-linux-gnu out/kgw_ehd_rv64 > out/ehd-riscv64.txt
expect out/ehd-riscv64.txt "${fix}/glibc-fma.txt" "glibc riscv64 (qemu)"
if command -v docker > /dev/null; then
    for img in python:3.11-slim-bullseye python:3.12-slim-bookworm; do  # glibc 2.31, 2.36
        tag="${img//[:\/]/_}"
        docker run --rm -v "${tools}:/w:ro" "${img}" python3 -I /w/powdump.py > "out/pow-${tag}-default.txt"
        # glibc 2.31 spells the tunable -AVX2_Usable,-FMA_Usable; 2.33+ spells it -AVX2,-FMA.
        docker run --rm -e GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2_Usable,-FMA_Usable,-AVX2,-FMA \
            -v "${tools}:/w:ro" "${img}" python3 -I /w/powdump.py > "out/pow-${tag}-masked.txt"
        same_col "out/pow-${tag}-default.txt" "${fix}/glibc-fma.txt" && echo "PASS ${tag} default = glibc-fma.txt" || { echo "FAIL ${tag} default"; exit 1; }
        same_col "out/pow-${tag}-masked.txt" "${fix}/glibc-sse2.txt" && echo "PASS ${tag} masked = glibc-sse2.txt" || { echo "FAIL ${tag} masked"; exit 1; }
    done
fi
gcc -O1 -o out/winpow "${tools}/winpow.c"
for v in 23.1.8 24.0.0-rc.3; do
    pin="v${v%%.*}"
    va="$(awk -v p="${pin}-win64:" '$1 == p {print $NF}' out/check-listings.txt)"  # found by check_listings.py
    for cw in 37f:pc64 27f:pc53; do
        out/winpow "$(dashd "${v}" win64)" "${va}" "${cw%%:*}" > "out/pow-win64-${v}-${cw##*:}.txt"
        expect "out/pow-win64-${v}-${cw##*:}.txt" "${fix}/mingw-win64-${cw##*:}.txt" "win64 ${v} dashd.exe pow at ${va}, x87 control word 0x${cw%%:*}"
    done
done

echo "== KGW: the proof"
py "${tools}/kgw_proof.py" "${fix}/cr.txt" \
    linux-x86_64-glibc-fma="${fix}/glibc-fma.txt:sep" linux-x86_64-glibc-sse2="${fix}/glibc-sse2.txt:sep" \
    linux-aarch64-glibc="${fix}/glibc-fma.txt:fused" linux-riscv64-glibc="${fix}/glibc-fma.txt:fused" \
    win64-mingw-pc64="${fix}/mingw-win64-pc64.txt:sep" win64-mingw-pc53="${fix}/mingw-win64-pc53.txt:sep" | tee out/kgw-proof.txt
echo "done: ${work}"
