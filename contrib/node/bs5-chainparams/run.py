#!/usr/bin/env python3
"""Generate dash-params' Core chain-params corpus (node program BS5).

For every case (chain + Core command-line arguments) and both pins, run
chainparams_dump (Core's own CChainParams and AppInitMain's spork block, from
build.sh's controlled clean build of the pin's commit in pins.txt) and record
its JSON. Every dump binary must match its MANIFEST, and every output must
carry the MANIFEST's pin and commit, before either JSON section is accepted.
The MANIFEST must be of a fresh clean build: a build tree build.sh reused
(BS5_REUSE=1) is refused unless --allow-reused-build is given.
The "params" part of every case the oracle can take is checked against the
pinned archives (dash-oracle-sys example chainparams_cases). The BRR fixture
comes from the pinned archives' oracle_mn_payment (example mn_payment_cases).
The corpus files are written only after every check at both pins passed.

usage: run.py --cases-only CASES --mn-cases-only MN_CASES
       run.py --dumps DIR --oracle-jsonl FILE --mn-jsonl FILE --out DIR
              [--corpus DIR]
       run.py --verify-dump PIN --dumps DIR   (provenance check only)
       --allow-reused-build: accept dumps from a reused build tree (development)
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

CHAINS = ["main", "test", "devnet", "regtest"]

# The 12 override combinations, applied to each of the 4 chains. Options a
# chain does not read are ignored by Core (regtest-only, devnet-only).
COMBOS: list[tuple[str, list[str]]] = [
    ("none", []),
    (
        "testactivationheight",
        [
            "-testactivationheight=bip147@5",
            "-testactivationheight=bip34@6",
            "-testactivationheight=dersig@7",
            "-testactivationheight=cltv@8",
            "-testactivationheight=csv@9",
            "-testactivationheight=brr@10",
            "-testactivationheight=dip0001@11",
            "-testactivationheight=dip0008@12",
            "-testactivationheight=dip0024@13",
            "-testactivationheight=v19@14",
            "-testactivationheight=v20@1000",
            "-testactivationheight=mn_rr@1001",
            "-testactivationheight=bip34@+16",
        ],
    ),
    ("vbparams_3", ["-vbparams=testdummy:100:200"]),
    (
        "vbparams_4_6_9",
        [
            "-vbparams=testdummy:1:2:3",
            "-vbparams=v24:-1:9223372036854775807:350:10:8:6:5:0",
            "-vbparams=testdummy:7:8:9:20:15",
            "-vbparams=v24:-2:5:0:-1:-1:-1:-1:7",
        ],
    ),
    ("dip3params", ["-dip3params=105:106"]),
    ("budgetparams", ["-budgetparams=10:20:30"]),
    (
        "llmqtestparams",
        [
            "-llmqtestparams=5:3",
            "-llmqtestinstantsendparams=4:3",
            "-llmqtestplatformparams=6:4",
        ],
    ),
    (
        "llmqtestinstantsenddip0024",
        ["-llmqtestinstantsenddip0024=llmq_test_instantsend"],
    ),
    (
        "devnet_subsidy_diff",
        [
            "-minimumdifficultyblocks=1000",
            "-highsubsidyblocks=500",
            "-highsubsidyfactor=10",
        ],
    ),
    (
        "devnet_llmq_roles",
        [
            "-llmqchainlocks=llmq_50_60",
            "-llmqinstantsenddip0024=llmq_60_75",
            "-llmqplatform=llmq_100_67",
            "-llmqmnhf=llmq_400_85",
        ],
    ),
    (
        "devnet_llmq_spacing",
        ["-llmqdevnetparams=10:6", "-powtargetspacing=60"],
    ),
    (
        "last_value_and_atoi",
        [
            "-dip3params=1:2",
            "-dip3params=200:300",
            "-budgetparams=1:2:3",
            "-budgetparams=40:50:60",
            "-highsubsidyfactor=4294967297",
            "-minimumdifficultyblocks= 12abc",
            "-highsubsidyblocks=99999999999",
            "-powtargetspacing=+90",
            "-llmqtestparams=+7:-0",
            "-llmqdevnetparams=400:1",
        ],
    ),
]

# Devnet genesis: the -devnet name goes into the devnet genesis coinbase.
DEVNET_NAMES: list[tuple[str, list[str]]] = [
    ("devnet_unnamed", []),
    ("devnet_empty_name", ["-devnet="]),
    ("devnet_name_bs5", ["-devnet=bs5"]),
    ("devnet_name_long", ["-devnet=dash-node_42.local-test-network"]),
]

# Override values Core refuses, one case per throw site in chainparams.cpp
# (both pins), plus error-order and wrong-chain cases.
ERRORS: list[tuple[str, str, list[str]]] = [
    ("tah_format", "regtest", ["-testactivationheight=v20"]),
    ("tah_height_neg", "regtest", ["-testactivationheight=v20@-1"]),
    ("tah_height_max", "regtest", ["-testactivationheight=v20@2147483647"]),
    ("tah_height_junk", "regtest", ["-testactivationheight=v20@1x"]),
    ("tah_height_empty", "regtest", ["-testactivationheight=v20@"]),
    ("tah_name", "regtest", ["-testactivationheight=v21@5"]),
    ("tah_name_case", "regtest", ["-testactivationheight=V20@5"]),
    (
        "tah_second_bad",
        "regtest",
        ["-testactivationheight=v19@5", "-testactivationheight=dip0003@5"],
    ),
    ("vb_fields_1", "regtest", ["-vbparams=testdummy"]),
    ("vb_fields_2", "regtest", ["-vbparams=testdummy:1"]),
    ("vb_fields_5", "regtest", ["-vbparams=testdummy:1:2:3:4"]),
    ("vb_fields_7", "regtest", ["-vbparams=testdummy:1:2:3:4:5:6"]),
    ("vb_fields_10", "regtest", ["-vbparams=testdummy:1:2:3:4:5:6:7:8:9"]),
    ("vb_start", "regtest", ["-vbparams=testdummy:x:2"]),
    ("vb_timeout", "regtest", ["-vbparams=testdummy:1:2.5"]),
    ("vb_min_height", "regtest", ["-vbparams=testdummy:1:2:2147483648"]),
    ("vb_window", "regtest", ["-vbparams=testdummy:1:2:3: 4:5"]),
    ("vb_threshold_start", "regtest", ["-vbparams=testdummy:1:2:3:4:"]),
    ("vb_threshold_min", "regtest", ["-vbparams=testdummy:1:2:3:4:5:++6:7:8"]),
    ("vb_falloff", "regtest", ["-vbparams=testdummy:1:2:3:4:5:6:0x7:8"]),
    ("vb_use_ehf", "regtest", ["-vbparams=testdummy:1:2:3:4:5:6:7:true"]),
    (
        "vb_start_overflow",
        "regtest",
        ["-vbparams=testdummy:9223372036854775808:2"],
    ),
    ("vb_deployment", "regtest", ["-vbparams=nope:1:2"]),
    ("vb_deployment_bad_first", "regtest", ["-vbparams=nope:x:2"]),
    ("vb_evo_shares", "regtest", ["-vbparams=evo_shares:1:2"]),
    ("dip3_fields", "regtest", ["-dip3params=1"]),
    ("dip3_empty", "regtest", ["-dip3params="]),
    ("dip3_activation", "regtest", ["-dip3params=a:1"]),
    ("dip3_enforcement", "regtest", ["-dip3params=1:"]),
    ("budget_fields", "regtest", ["-budgetparams=1:2"]),
    ("budget_mn", "regtest", ["-budgetparams=-:2:3"]),
    ("budget_budget", "regtest", ["-budgetparams=1:+-2:3"]),
    ("budget_superblock", "regtest", ["-budgetparams=1:2:3:"]),
    ("budget_superblock_junk", "regtest", ["-budgetparams=1:2:3x"]),
    ("llmqtest_fields", "regtest", ["-llmqtestparams=3"]),
    ("llmqtest_size", "regtest", ["-llmqtestparams=x:2"]),
    ("llmqtest_threshold", "regtest", ["-llmqtestparams=3:x"]),
    ("llmqtest_size_401", "regtest", ["-llmqtestparams=401:2"]),
    ("llmqtest_size_401_bad_threshold", "regtest", ["-llmqtestparams=401:x"]),
    ("llmqtestis_fields", "regtest", ["-llmqtestinstantsendparams=3:2:1"]),
    ("llmqtestis_size", "regtest", ["-llmqtestinstantsendparams=:2"]),
    ("llmqtestis_size_401", "regtest", ["-llmqtestinstantsendparams=401:2"]),
    ("llmqtestplat_threshold", "regtest", ["-llmqtestplatformparams=3:2b"]),
    (
        "llmqtestisdip0024_name",
        "regtest",
        ["-llmqtestinstantsenddip0024=llmq_50_60"],
    ),
    ("llmqtestisdip0024_empty", "regtest", ["-llmqtestinstantsenddip0024="]),
    ("dev_chainlocks_rotated", "devnet", ["-llmqchainlocks=llmq_60_75"]),
    ("dev_chainlocks_name", "devnet", ["-llmqchainlocks=llmq_test"]),
    (
        "dev_isdip0024_unrotated",
        "devnet",
        ["-llmqinstantsenddip0024=llmq_50_60"],
    ),
    ("dev_isdip0024_name", "devnet", ["-llmqinstantsenddip0024=x"]),
    ("dev_platform_name", "devnet", ["-llmqplatform=llmq_25_67"]),
    ("dev_mnhf_name", "devnet", ["-llmqmnhf="]),
    ("dev_llmqdevnet_fields", "devnet", ["-llmqdevnetparams=12"]),
    ("dev_llmqdevnet_size", "devnet", ["-llmqdevnetparams=a:6"]),
    ("dev_llmqdevnet_threshold", "devnet", ["-llmqdevnetparams=12:"]),
    ("dev_llmqdevnet_size_401", "devnet", ["-llmqdevnetparams=401:6"]),
    ("dev_spacing_parse", "devnet", ["-powtargetspacing=2.5"]),
    ("dev_spacing_zero", "devnet", ["-powtargetspacing=0"]),
    ("dev_spacing_negative", "devnet", ["-powtargetspacing=-60"]),
    # Core's errors after an LLMQ size outside u16, which dash-params refuses
    # only once Core's own checks pass (quirk 46).
    (
        "llmq_range_then_dip0024",
        "regtest",
        ["-llmqtestparams=-1:2", "-llmqtestinstantsenddip0024=bogus"],
    ),
    (
        "llmq_devnet_range_then_spacing",
        "devnet",
        ["-llmqdevnetparams=-2:9", "-powtargetspacing=abc"],
    ),
    (
        "llmq_range_then_is_threshold",
        "regtest",
        ["-llmqtestparams=-1:2", "-llmqtestinstantsendparams=1:2b"],
    ),
    (
        "order_regtest",
        "regtest",
        [
            "-llmqtestparams=x",
            "-budgetparams=x",
            "-dip3params=x",
            "-vbparams=x",
            "-testactivationheight=x",
        ],
    ),
    (
        "order_regtest_after_tah",
        "regtest",
        [
            "-llmqtestinstantsenddip0024=x",
            "-llmqtestplatformparams=x",
            "-llmqtestinstantsendparams=x",
            "-llmqtestparams=x",
            "-budgetparams=x",
            "-dip3params=x",
            "-vbparams=x",
        ],
    ),
    (
        "order_devnet",
        "devnet",
        [
            "-powtargetspacing=x",
            "-llmqdevnetparams=x",
            "-llmqmnhf=x",
            "-llmqplatform=x",
            "-llmqinstantsenddip0024=x",
            "-llmqchainlocks=x",
        ],
    ),
    (
        "order_devnet_late",
        "devnet",
        ["-powtargetspacing=x", "-llmqdevnetparams=x", "-llmqmnhf=x"],
    ),
    (
        "ignored_off_chain_main",
        "main",
        [
            "-dip3params=x",
            "-llmqchainlocks=x",
            "-powtargetspacing=x",
            "-testactivationheight=x",
        ],
    ),
    (
        "ignored_off_chain_regtest",
        "regtest",
        ["-llmqchainlocks=x", "-powtargetspacing=0", "-llmqdevnetparams=x"],
    ),
    ("ignored_off_chain_devnet", "devnet", ["-dip3params=x", "-vbparams=x"]),
]

# Settings semantics, devnet names long enough for PUSHDATA1/PUSHDATA2, values
# at the limits Core accepts, and Core errors after a negative height (which
# dash-params refuses only once Core's own checks pass).
EXTRA: list[tuple[str, str, list[str]]] = [
    ("neg_vbparams_after", "regtest", ["-vbparams=testdummy:1:2", "-novbparams"]),
    (
        "neg_vbparams_before",
        "regtest",
        ["-vbparams=x", "-novbparams", "-vbparams=testdummy:3:4"],
    ),
    ("neg_vbparams_double", "regtest", ["-novbparams=0"]),
    (
        "neg_tah",
        "regtest",
        [
            "-testactivationheight=v19@5",
            "-notestactivationheight",
            "-testactivationheight=dip0001@7",
        ],
    ),
    ("neg_tah_last", "regtest", ["-testactivationheight=x", "-notestactivationheight"]),
    ("neg_dip3", "regtest", ["-dip3params=1:2", "-nodip3params"]),
    ("neg_dip3_double", "regtest", ["-nodip3params=0"]),
    ("neg_llmqchainlocks", "devnet", ["-nollmqchainlocks"]),
    ("neg_devnet_name", "devnet", ["-nodevnet"]),
    ("neg_highsubsidyfactor", "devnet", ["-highsubsidyfactor=5", "-nohighsubsidyfactor"]),
    ("devnet_name_80", "devnet", ["-devnet=" + "n" * 80]),
    ("devnet_name_300", "devnet", ["-devnet=" + "x" * 300]),
    ("limit_llmq_400", "regtest", ["-llmqtestparams=400:2"]),
    (
        "limit_tah_max",
        "regtest",
        [
            "-testactivationheight=v20@2147483646",
            "-testactivationheight=mn_rr@2147483646",
        ],
    ),
    ("limit_llmq_odd", "regtest", ["-llmqtestparams=0:5"]),
    (
        "limit_devnet_negative",
        "devnet",
        ["-highsubsidyfactor=-7", "-minimumdifficultyblocks=-1"],
    ),
    ("llmqtestparams_ignored_main", "main", ["-llmqtestparams=-1:2"]),
    ("llmqtestparams_ignored_test", "test", ["-llmqtestparams=-1:2"]),
    ("llmqdevnetparams_ignored_main", "main", ["-llmqdevnetparams=-2:9"]),
    ("llmqdevnetparams_ignored_test", "test", ["-llmqdevnetparams=-2:9"]),
    ("llmqdevnetparams_ignored_regtest", "regtest", ["-llmqdevnetparams=-2:9"]),
    (
        "llmqtestparams_ignored_devnet",
        "devnet",
        [
            "-llmqtestparams=-1:2",
            "-llmqtestinstantsendparams=-3:-4",
            "-llmqtestplatformparams=3:70000",
        ],
    ),
    ("limit_spacing_big", "devnet", ["-powtargetspacing=86401"]),
    (
        "negative_then_budget_error",
        "regtest",
        ["-dip3params=-1:600", "-budgetparams=x"],
    ),
    (
        "negative_then_vb_error",
        "regtest",
        ["-vbparams=v24:0:9:-1", "-vbparams=nope:1:2"],
    ),
    (
        "negative_then_llmq_error",
        "regtest",
        ["-budgetparams=-1:2:3", "-llmqtestparams=x"],
    ),
    (
        "vb_negative_overwritten",
        "regtest",
        ["-vbparams=v24:0:9:-1", "-vbparams=v24:0:9:0"],
    ),
    (
        "vb_negative_overwritten_3",
        "regtest",
        ["-vbparams=v24:0:9:-1", "-vbparams=v24:0:9"],
    ),
    ("dip3_negative_overwritten", "regtest", ["-dip3params=-1:5", "-dip3params=4:5"]),
    ("neg_vbparams_garbage", "regtest", ["-novbparams=garbage"]),
    ("neg_dip3_0abc", "regtest", ["-nodip3params=0abc"]),
    ("neg_dip3_true", "regtest", ["-nodip3params=1"]),
]

# Inputs Core accepts but dash-params refuses: a height still negative in
# the final parameters (BL-31), and an LLMQ size or threshold outside u16
# (quirk 46). Core's dump is kept, with the refusal and its registered
# deviation, at the pins where Core starts (v24 refuses sizes above 400).
REFUSED: list[tuple[str, str, list[str], str, int | None]] = [
    (
        "vb_other_negative",
        "regtest",
        ["-vbparams=testdummy:0:9:-5", "-vbparams=v24:0:9:-1", "-vbparams=v24:0:9:0"],
        "negative height -5 in -vbparams",
        None,
    ),
    ("budget_negative", "regtest", ["-budgetparams=10:-1:30"], "negative height -1 in -budgetparams", None),
    ("dip3_negative", "regtest", ["-dip3params=-1:5"], "negative height -1 in -dip3params", None),
    (
        "llmq_negative",
        "regtest",
        ["-llmqtestinstantsendparams=-3:-4"],
        "LLMQ_TEST_INSTANTSEND size (-3) or threshold (-4) outside 0 to 65535",
        46,
    ),
    (
        "llmq_devnet_negative",
        "devnet",
        ["-llmqdevnetparams=-2:9"],
        "LLMQ_DEVNET size (-2) or threshold (9) outside 0 to 65535",
        46,
    ),
    (
        "llmq_threshold_70000",
        "regtest",
        ["-llmqtestparams=3:70000"],
        "LLMQ_TEST size (3) or threshold (70000) outside 0 to 65535",
        46,
    ),
    (
        "llmq_threshold_negative",
        "regtest",
        ["-llmqtestplatformparams=3:-1"],
        "LLMQ_TEST_PLATFORM size (3) or threshold (-1) outside 0 to 65535",
        46,
    ),
]

# Inputs whose outcome differs by pin: Core refuses them at one and starts at
# the other, where dash-params refuses them (v23.1 has no LLMQ size cap and
# v24 no -minsporkkeys). The corpus names each by its outcome at the pin,
# "error/<name>" or "refused/<name>"; tool_only marks the dump-only ones.
BY_OUTCOME: list[tuple[str, str, list[str], str, int | None, bool]] = [
    (
        "llmq_size_70000",
        "regtest",
        ["-llmqtestparams=70000:2"],
        "LLMQ_TEST size (70000) or threshold (2) outside 0 to 65535",
        46,
        False,
    ),
    (
        "llmqtestplat_size_1000000",
        "regtest",
        ["-llmqtestplatformparams=1000000:2"],
        "LLMQ_TEST_PLATFORM size (1000000) or threshold (2) outside 0 to 65535",
        46,
        False,
    ),
    (
        "llmq_range_then_min_spork_keys",
        "regtest",
        ["-llmqtestparams=-1:2", "-minsporkkeys=1"],
        "LLMQ_TEST size (-1) or threshold (2) outside 0 to 65535",
        46,
        True,
    ),
]

# Inputs Core's argument parser (ParseParameters) refuses on every chain,
# before it picks one: -powtargetspacing is registered DISALLOW_NEGATION, so
# even the double negative -nopowtargetspacing=0 is refused. The oracle
# reports these as "ParseParameters: <Core's text>".
PARSE_ERRORS: list[tuple[str, str, list[str]]] = [
    ("neg_powtargetspacing_main", "main", ["-nopowtargetspacing"]),
    ("neg_powtargetspacing_test", "test", ["-nopowtargetspacing"]),
    ("neg_powtargetspacing_regtest", "regtest", ["-nopowtargetspacing"]),
    ("neg_powtargetspacing_devnet", "devnet", ["-nopowtargetspacing"]),
    ("neg_powtargetspacing_double", "devnet", ["-nopowtargetspacing=0"]),
    ("neg_powtargetspacing_double_main", "main", ["-nopowtargetspacing=0"]),
    (
        "neg_powtargetspacing_after_value",
        "devnet",
        ["-powtargetspacing=60", "-nopowtargetspacing"],
    ),
    (
        "neg_powtargetspacing_after_chain_errors",
        "regtest",
        ["-dip3params=x", "-llmqtestparams=-1:2", "-nopowtargetspacing"],
    ),
]

REGTEST_SIGNER = "yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW"
TEST_SIGNER = "yjPtiKh2uwk3bDutTEA2q9mCtXyiZRWn55"
MAIN_SIGNER = "Xgtyuk76vhuFW2iT7UAiHgNdWXCf3J34wh"
TEST_P2SH = "8eRTi4fUVRoeYEeeTyL4DPAwxatvbjaxFV"
TEST_OTHER = "yLKU4EJxjbv8peagVRM3UykZDJoaUUrXSn"

# -sporkaddr and -minsporkkeys (init.cpp options the oracle does not
# register, so dump-tool only): AppInitMain's resolution after the params.
SPORK: list[tuple[str, str, list[str]]] = [
    ("custom", "regtest", ["-sporkaddr=" + TEST_SIGNER]),
    ("main_default_again", "main", ["-sporkaddr=" + MAIN_SIGNER]),
    ("invalid", "regtest", ["-sporkaddr=invalid"]),
    ("wrong_network", "regtest", ["-sporkaddr=" + MAIN_SIGNER]),
    ("script_hash", "regtest", ["-sporkaddr=" + TEST_P2SH]),
    ("whitespace", "regtest", ["-sporkaddr= " + TEST_SIGNER + "\t"]),
    ("inner_space", "regtest", ["-sporkaddr=yjPtiKh2uwk3bDut TEA2q9mCtXyiZRWn55"]),
    ("platform_hrp", "regtest", ["-sporkaddr=tdash1qq"]),
    ("two", "regtest", ["-sporkaddr=" + REGTEST_SIGNER, "-sporkaddr=" + TEST_SIGNER]),
    (
        "two_threshold_2",
        "regtest",
        ["-sporkaddr=" + REGTEST_SIGNER, "-sporkaddr=" + TEST_SIGNER, "-minsporkkeys=2"],
    ),
    (
        "three_threshold_2",
        "test",
        [
            "-sporkaddr=" + REGTEST_SIGNER,
            "-sporkaddr=" + TEST_SIGNER,
            "-sporkaddr=" + TEST_OTHER,
            "-minsporkkeys=2",
        ],
    ),
    ("duplicate", "regtest", ["-sporkaddr=" + TEST_SIGNER, "-sporkaddr=" + TEST_SIGNER]),
    ("negated", "regtest", ["-nosporkaddr"]),
    (
        "negated_then_set",
        "regtest",
        ["-sporkaddr=invalid", "-nosporkaddr", "-sporkaddr=" + TEST_SIGNER],
    ),
    ("minkeys_0", "regtest", ["-minsporkkeys=0"]),
    ("minkeys_1", "devnet", ["-minsporkkeys=1"]),
    ("minkeys_garbage", "regtest", ["-minsporkkeys=garbage"]),
    ("minkeys_wrap", "regtest", ["-minsporkkeys=4294967297"]),
    ("minkeys_negated", "regtest", ["-nominsporkkeys"]),
    ("chain_error_first", "regtest", ["-sporkaddr=invalid", "-dip3params=x"]),
    ("minkeys_before_chain", "regtest", ["-minsporkkeys=1", "-dip3params=x"]),
    # Spork errors come before an LLMQ override outside u16 (quirk 46).
    ("llmq_range_then_invalid", "regtest", ["-llmqtestparams=-1:2", "-sporkaddr=invalid"]),
    ("llmq_range_then_minkeys", "regtest", ["-llmqtestparams=-1:2", "-minsporkkeys=2"]),
    # ParseParameters reports the first argument it refuses.
    ("parse_negation_first", "regtest", ["-nopowtargetspacing", "-minsporkkeys=1"]),
    ("parse_minkeys_first", "regtest", ["-minsporkkeys=1", "-nopowtargetspacing"]),
]

# Cases only the dump tool can run: -fastprune is registered by dashd's init,
# not by SetupChainParamsOptions, so the oracle's ParseParameters refuses it.
TOOL_ONLY: list[tuple[str, str, list[str]]] = [
    ("regtest_fastprune", "regtest", ["-fastprune"]),
    ("regtest_fastprune_0", "regtest", ["-fastprune=0"]),
    ("regtest_fastprune_empty", "regtest", ["-fastprune="]),
    ("regtest_fastprune_neg", "regtest", ["-nofastprune"]),
    ("regtest_fastprune_atoi", "regtest", ["-fastprune= 2x"]),
    ("main_fastprune", "main", ["-fastprune"]),
    ("regtest_fastprune_double_neg", "regtest", ["-nofastprune=0"]),
    ("regtest_fastprune_garbage", "regtest", ["-nofastprune=garbage"]),
    ("regtest_fastprune_0x1", "regtest", ["-nofastprune=0x1"]),
    ("regtest_fastprune_0abc", "regtest", ["-nofastprune=0abc"]),
]


def cases() -> list[dict]:
    out = []
    for chain in CHAINS:
        for name, args in COMBOS:
            out.append({"name": f"{chain}/{name}", "chain": chain, "args": args})
    for name, args in DEVNET_NAMES:
        out.append({"name": name, "chain": "devnet", "args": args})
    for name, chain, args in ERRORS:
        out.append({"name": f"error/{name}", "chain": chain, "args": args})
    for name, chain, args in EXTRA:
        out.append({"name": f"extra/{name}", "chain": chain, "args": args})
    for name, chain, args, _, _ in REFUSED:
        out.append({"name": f"refused/{name}", "chain": chain, "args": args})
    for name, chain, args in PARSE_ERRORS:
        out.append({"name": f"error/{name}", "chain": chain, "args": args})
    for name, chain, args, _, _, tool_only in BY_OUTCOME:
        if not tool_only:
            out.append({"name": f"outcome/{name}", "chain": chain, "args": args})
    return out


def tool_cases() -> list[dict]:
    out = [
        {"name": n, "chain": c, "args": a, "tool_only": True}
        for n, c, a in TOOL_ONLY
    ]
    for n, c, a in SPORK:
        out.append({"name": f"spork/{n}", "chain": c, "args": a, "tool_only": True})
    for n, c, a, _, _, tool_only in BY_OUTCOME:
        if tool_only:
            out.append({"name": f"outcome/{n}", "chain": c, "args": a, "tool_only": True})
    return out


# BRR periods: Core's GetMasternodePayment at mainnet, classic era, around
# every period boundary and well past the last one.
BRR_START = 1374912 - 1374912 % 16616 + 16616
BRR_CYCLE = 3 * 16616
BRR_VALUE = 1_000_000_000


def mn_cases() -> list[dict]:
    heights = [BRR_START - 1]
    for i in range(19):
        heights += [BRR_START + i * BRR_CYCLE, BRR_START + (i + 1) * BRR_CYCLE - 1]
    heights.append(BRR_START + 40 * BRR_CYCLE)
    return [{"height": h, "block_value": BRR_VALUE} for h in heights]


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pinned_commits(repo: Path) -> dict[str, str]:
    out = {}
    for line in (repo / "dash-oracle-sys/pins.txt").read_text().splitlines():
        f = line.split()
        if f and not line.startswith("#"):
            out[f[0]] = f[1]
    return out


def manifest(path: Path) -> dict[str, list[str]]:
    out: dict[str, list[str]] = {}
    for line in path.read_text().splitlines():
        key, _, value = line.partition(" ")
        out.setdefault(key, []).append(value)
    return out


def verify_dump(pin: str, dumps: Path, dash_src: Path, allow_reused: bool) -> dict:
    """The dump binary of `pin` with its verified provenance, or exit."""
    here = Path(__file__).resolve().parent
    commit = pinned_commits(here.parents[2]).get(pin)
    binary = dumps / f"chainparams_dump_{pin}"
    m = manifest(dumps / f"MANIFEST-{pin}")
    tree = subprocess.run(
        ["git", "-C", str(dash_src), "rev-parse", f"{commit}^{{tree}}"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    checks = {
        "pin": m.get("pin") == [pin],
        "commit in pins.txt": commit is not None and m.get("commit") == [commit],
        "tree": m.get("tree") == [tree],
        "fresh build": m.get("build") == ["fresh"]
        or (allow_reused and m.get("build") == ["reused"]),
        "anchor": len(m.get("anchor", [])) == 1,
        "dump source": m.get("source", [""])[0].split(" ")[0]
        == sha256(here / "chainparams_dump.cpp"),
        "binary": m.get("binary") == [sha256(binary)],
    }
    bad = [k for k, ok in checks.items() if not ok]
    if bad:
        raise SystemExit(f"{pin}: provenance check failed: {', '.join(bad)}")
    return {"pin": pin, "commit": commit, "tree": tree, "binary": str(binary)}


def run_tool(dump: dict, case: dict) -> dict:
    proc = subprocess.run(
        [dump["binary"], case["chain"], *case["args"]],
        check=True,
        capture_output=True,
        text=True,
    )
    got = json.loads(proc.stdout)
    prov = got.get("provenance", {})
    if prov.get("pin") != dump["pin"] or prov.get("commit") != dump["commit"]:
        raise SystemExit(f"{dump['pin']} {case['name']}: output provenance {prov}")
    return got


def oracle_results(path: Path) -> dict[tuple[str, str], dict]:
    out = {}
    for line in path.read_text().splitlines():
        rec = json.loads(line)
        out[(rec["pin"], rec["name"])] = rec
    return out


def strip_version(params: dict) -> dict:
    return {k: v for k, v in params.items() if k != "core_version"}


def flatten(value: object, prefix: str = "") -> dict[str, object]:
    """Leaf values of a JSON value keyed by dotted paths."""
    if isinstance(value, dict):
        out: dict[str, object] = {}
        for k, v in value.items():
            out.update(flatten(v, f"{prefix}{k}."))
        return out
    if isinstance(value, list):
        out = {f"{prefix}#": len(value)}
        for i, v in enumerate(value):
            out.update(flatten(v, f"{prefix}{i}."))
        return out
    return {prefix[:-1]: value}


HEADER = """\
// Dash Core's chain parameters, {pin} pin ({version}), for dash-params' builder
// equality test (node program BS5). Produced by Core, not written by hand:
// contrib/node/bs5-chainparams in rust-dashcore builds chainparams_dump from a
// controlled clean build of Core commit {commit} (tree {tree}), the pin in
// dash-oracle-sys/pins.txt, and runs each case through ClearArgs,
// SetupChainParamsOptions, ParseParameters and SelectParams, then
// AppInitMain's spork block. Dump binary sha256 {binary}.
// Every case but the "tool_only" ones was checked equal to the pinned
// archive's oracle_chainparams_json (or its error text) by the same script.
//
// "bases" holds the full dump of each chain with no arguments: "params" is
// the oracle's ChainParamsJson(), "ext" the CChainParams members it omits and
// "spork" the resolved spork signers. A case holds Core's error text
// ("error"), or the leaves that differ from its chain's base ("delta"), keyed
// by dotted path; "#" is an array length. Keys are the same set in every
// case. A "refused" case is one Core accepts and dash-params refuses, with
// the refusal's text and, for a registered deviation, its quirk number
// ("deviation"; 46: LLMQ sizes outside u16). core_version is dropped.
"""

MN_HEADER = """\
// Dash Core's masternode payment at mainnet in the classic reward era,
// GetMasternodePayment(height, {value}, Classic), around every block reward
// reallocation period boundary, for dash-params' BRR table test (node program
// BS5). Produced by the pinned oracle archives' oracle_mn_payment
// (dash-oracle-sys example mn_payment_cases), commits {commits}.
"""


PARSE_ERROR_NAMES = {f"error/{name}" for name, _, _ in PARSE_ERRORS}
PARSE_PREFIX = "ParseParameters: "


def record(dump: dict, case: dict, got: dict) -> dict:
    rec = {k: case[k] for k in ("name", "chain", "args")}
    if case.get("tool_only"):
        rec["tool_only"] = True
    if "parse_error" in got:
        if not case.get("tool_only") and case["name"] not in PARSE_ERROR_NAMES:
            raise SystemExit(f"{dump['pin']} {case['name']}: {got['parse_error']}")
        rec["error"] = got["parse_error"]
    elif "error" in got:
        rec["error"] = got["error"]
    elif "error" in got["spork"]:
        rec["error"] = got["spork"]["error"]
    else:
        rec["params"] = strip_version(got["params"])
        rec["ext"] = got["ext"]
        rec["spork"] = got["spork"]
    return rec


def corpus_text(dump: dict, version: str, binary: str, records: list[dict]) -> str:
    bases = {}
    for rec in records:
        if rec["name"].endswith("/none"):
            bases[rec["chain"]] = {k: rec[k] for k in ("params", "ext", "spork")}
    refused = {f"refused/{n}": (text, dev) for n, _, _, text, dev in REFUSED}
    refused.update({f"outcome/{n}": (text, dev) for n, _, _, text, dev, _ in BY_OUTCOME})
    cases = []
    for rec in records:
        case = {k: rec[k] for k in ("name", "chain", "args")}
        if case["name"].startswith("outcome/"):
            kind = "error" if "error" in rec else "refused"
            case["name"] = kind + case["name"].removeprefix("outcome")
        if rec.get("tool_only"):
            case["tool_only"] = True
        if "error" in rec:
            case["error"] = rec["error"]
        else:
            base = flatten(bases[rec["chain"]])
            got = flatten({k: rec[k] for k in ("params", "ext", "spork")})
            if base.keys() != got.keys():
                extra = sorted(base.keys() ^ got.keys())
                case["extra_keys"] = {k: got[k] for k in extra if k in got}
                case["missing_keys"] = [k for k in extra if k not in got]
            case["delta"] = {k: v for k, v in got.items() if k in base and base[k] != v}
        if rec["name"] in refused and "error" not in rec:
            text, deviation = refused[rec["name"]]
            case["refused"] = text
            if deviation is not None:
                case["deviation"] = deviation
        cases.append(case)
    head = HEADER.format(
        pin=dump["pin"], version=version, commit=dump["commit"], tree=dump["tree"], binary=binary
    )
    body = json.dumps({"core_version": version, "bases": bases, "cases": cases}, indent=1)
    return head + body + "\n"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases-only")
    ap.add_argument("--mn-cases-only")
    ap.add_argument("--verify-dump")
    ap.add_argument("--dumps")
    ap.add_argument("--dash-src", default=str(Path.home() / "workspace/dash"))
    ap.add_argument("--oracle-jsonl")
    ap.add_argument("--mn-jsonl")
    ap.add_argument("--out")
    ap.add_argument("--corpus")
    ap.add_argument("--allow-reused-build", action="store_true")
    a = ap.parse_args()
    if a.cases_only or a.mn_cases_only:
        if a.cases_only:
            Path(a.cases_only).write_text(json.dumps(cases(), indent=1))
        if a.mn_cases_only:
            Path(a.mn_cases_only).write_text(json.dumps(mn_cases(), indent=1))
        return
    dumps, dash_src = Path(a.dumps), Path(a.dash_src)
    if a.verify_dump:
        dump = verify_dump(a.verify_dump, dumps, dash_src, a.allow_reused_build)
        run_tool(dump, {"name": "probe", "chain": "main", "args": []})
        print(f"{a.verify_dump}: provenance ok")
        return
    oracle = oracle_results(Path(a.oracle_jsonl))
    mismatches = 0
    texts: dict[str, str] = {}
    pins = sorted(pinned_commits(Path(__file__).resolve().parents[3]))
    for pin in pins:
        dump = verify_dump(pin, dumps, dash_src, a.allow_reused_build)
        binary = sha256(Path(dump["binary"]))
        records = []
        version = None
        for case in cases() + tool_cases():
            got = run_tool(dump, case)
            rec = record(dump, case, got)
            if not case.get("tool_only"):
                o = oracle[(pin, case["name"])]
                if "params" in o:
                    version = o["params"]["core_version"]
                want = (
                    {"error": o["error"]}
                    if "error" in o
                    else {"params": strip_version(o["params"])}
                )
                if case["name"] in PARSE_ERROR_NAMES:
                    error = want.get("error", "")
                    want = {"error": error.removeprefix(PARSE_PREFIX)} if error.startswith(PARSE_PREFIX) else want
                have = (
                    {"error": rec["error"]} if "error" in rec else {"params": rec["params"]}
                )
                if want != have:
                    mismatches += 1
                    print(f"MISMATCH {pin} {case['name']}")
            records.append(rec)
        slug = pin.replace(".", "_")
        out = Path(a.out) / f"chainparams_{slug}.json"
        out.write_text(json.dumps({"core_version": version, "cases": records}, indent=1))
        texts[f"chainparams_{slug}.json5"] = corpus_text(dump, version, binary, records)
        n_err = sum("error" in r for r in records)
        print(f"{pin}: {len(records)} cases ({n_err} errors) -> {out}")
    mn = [json.loads(line) for line in Path(a.mn_jsonl).read_text().splitlines()]
    if len(mn) != len(pins) * len(mn_cases()) or any("payment" not in r for r in mn):
        raise SystemExit("oracle_mn_payment results incomplete")
    commits = ", ".join(f"{p} {c}" for p, c in sorted(pinned_commits(Path(__file__).resolve().parents[3]).items()))
    texts["brr_payments.json5"] = MN_HEADER.format(value=BRR_VALUE, commits=commits) + json.dumps(
        {"realloc_start": BRR_START, "period_blocks": BRR_CYCLE, "payments": mn}, indent=1
    ) + "\n"
    if mismatches:
        raise SystemExit(f"{mismatches} cases differ from the pinned oracle; corpus not written")
    print("all oracle cases equal the pinned oracle_chainparams_json")
    if a.corpus:
        with tempfile.TemporaryDirectory(dir=a.corpus) as tmp:
            for name, text in texts.items():
                (Path(tmp) / name).write_text(text)
            for name in texts:
                (Path(tmp) / name).replace(Path(a.corpus) / name)
        print(f"corpus written to {a.corpus}")


if __name__ == "__main__":
    main()
