// Dumps Dash Core's CoinJoin denomination, collateral and rebalance helpers
// (src/coinjoin/common.h) over a fixed input grid as JSON, for dash-coinjoin's
// tests (CJ0). Built against the pinned Core sources by regen.sh; nothing here
// re-implements a Core function. Header-only: every function used is constexpr
// (DenominationToString needs core_write.cpp's ValueFromAmount, so its strings
// come from dashd instead; see regen.sh).
#include <coinjoin/common.h>
#include <consensus/amount.h>

#include <climits>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

int main()
{
    std::vector<int> denoms;
    for (int d = -2; d <= 40; ++d) denoms.push_back(d);
    for (int d : {63, 64, 65, 127, 128, 255, 256, 1 << 20, INT_MAX, INT_MIN, INT_MIN + 1, -16, -32}) denoms.push_back(d);

    std::vector<CAmount> amounts{0, -1, 1, COIN - 1, COIN, COIN + 1, MAX_MONEY, MAX_MONEY + 1, -COIN, 20 * COIN,
                                 123456789, 5 * COIN + 3, 999, 1000, 1001, INT64_MAX, INT64_MIN};
    for (CAmount a : CoinJoin::GetStandardDenominations()) {
        for (CAmount delta : {-1, 0, 1}) amounts.push_back(a + delta);
        amounts.push_back(a * 2);
        amounts.push_back(a * 10);
    }
    for (CAmount a : {CoinJoin::GetCollateralAmount(), CoinJoin::GetMaxCollateralAmount()}) {
        for (CAmount delta : {-1, 0, 1}) amounts.push_back(a + delta);
    }

    std::printf("{\n");
    std::printf("  \"standard_denominations\": [");
    for (size_t i = 0; i < CoinJoin::GetStandardDenominations().size(); ++i) {
        std::printf("%s%lld", i ? ", " : "", (long long)CoinJoin::GetStandardDenominations()[i]);
    }
    std::printf("],\n");
    std::printf("  \"smallest_denomination\": %lld,\n", (long long)CoinJoin::GetSmallestDenomination());
    std::printf("  \"collateral_amount\": %lld,\n", (long long)CoinJoin::GetCollateralAmount());
    std::printf("  \"max_collateral_amount\": %lld,\n", (long long)CoinJoin::GetMaxCollateralAmount());
    std::printf("  \"promotion_ratio\": %d,\n", CoinJoin::PROMOTION_RATIO);
    std::printf("  \"gap_divisor\": %d,\n", CoinJoin::GAP_DIVISOR);
    std::printf("  \"min_gap_threshold\": %d,\n", CoinJoin::MIN_GAP_THRESHOLD);

    // [denom, index, to_amount, is_valid, larger_adjacent, smaller_adjacent]
    std::printf("  \"denoms\": [\n");
    for (size_t i = 0; i < denoms.size(); ++i) {
        const int d = denoms[i];
        std::printf("    [%d, %d, %lld, %s, %d, %d]%s\n", d, CoinJoin::GetDenominationIndex(d),
                    (long long)CoinJoin::DenominationToAmount(d), CoinJoin::IsValidDenomination(d) ? "true" : "false",
                    CoinJoin::GetLargerAdjacentDenom(d),
                    CoinJoin::GetSmallerAdjacentDenom(d), i + 1 < denoms.size() ? "," : "");
    }
    std::printf("  ],\n");

    // [d1, d2, adjacent]
    std::printf("  \"adjacent\": [\n");
    bool first = true;
    for (int a = -1; a <= 17; ++a) {
        for (int b = -1; b <= 17; ++b) {
            std::printf("%s    [%d, %d, %s]", first ? "" : ",\n", a, b,
                        CoinJoin::AreAdjacentDenominations(a, b) ? "true" : "false");
            first = false;
        }
    }
    std::printf("\n  ],\n");

    // [amount, to_denomination, is_denominated, is_collateral, priority]
    std::printf("  \"amounts\": [\n");
    for (size_t i = 0; i < amounts.size(); ++i) {
        const CAmount a = amounts[i];
        std::printf("    [%lld, %d, %s, %s, %d]%s\n", (long long)a, CoinJoin::AmountToDenomination(a),
                    CoinJoin::IsDenominatedAmount(a) ? "true" : "false",
                    CoinJoin::IsCollateralAmount(a) ? "true" : "false", CoinJoin::CalculateAmountPriority(a),
                    i + 1 < amounts.size() ? "," : "");
    }
    std::printf("  ],\n");

    // [goal, gap_threshold]
    std::printf("  \"gap_threshold\": [\n");
    first = true;
    for (int g : {-100, -6, -5, -1, 0, 1, 4, 5, 9, 10, 11, 24, 25, 26, 29, 30, 49, 50, 51, 99, 100, 300, 100000, INT_MAX}) {
        std::printf("%s    [%d, %d]", first ? "" : ",\n", g, CoinJoin::GetGapThreshold(g));
        first = false;
    }
    std::printf("\n  ],\n");

    // [a, b, fully_mixed, goal, should_promote(a=smaller, b=larger), should_demote(a=larger, b=smaller)]
    std::printf("  \"rebalance\": [\n");
    first = true;
    const int counts[] = {0, 5, 9, 10, 24, 25, 26, 50, 51, 100};
    const int mixed[] = {0, 1, 9, 10, 11};
    const int goals[] = {10, 24, 25, 50, 100, 300};
    for (int a : counts) {
        for (int b : counts) {
            for (int m : mixed) {
                for (int g : goals) {
                    std::printf("%s    [%d, %d, %d, %d, %s, %s]", first ? "" : ",\n", a, b, m, g,
                                CoinJoin::ShouldPromoteDenoms(a, b, m, g) ? "true" : "false",
                                CoinJoin::ShouldDemoteDenoms(a, b, m, g) ? "true" : "false");
                    first = false;
                }
            }
        }
    }
    std::printf("\n  ],\n");

    // Every sequence of up to four shapes (0 UNKNOWN, 1 STANDARD, 2 PROMOTION, 3 DEMOTION):
    // [shapes, inputs, outputs, covered]
    std::printf("  \"sides\": [\n");
    first = true;
    for (int n = 0; n <= 4; ++n) {
        int total = 1;
        for (int i = 0; i < n; ++i) total *= 4;
        for (int code = 0; code < total; ++code) {
            CoinJoin::MixSideCounts counts_acc;
            std::string shapes = "[";
            int c = code;
            for (int i = 0; i < n; ++i) {
                const int s = c % 4;
                c /= 4;
                counts_acc.Add(static_cast<CoinJoin::MixShape>(s));
                shapes += (i ? ", " : "") + std::to_string(s);
            }
            shapes += "]";
            std::printf("%s    [%s, %d, %d, %s]", first ? "" : ",\n", shapes.c_str(), counts_acc.inputs,
                        counts_acc.outputs, counts_acc.IsCovered() ? "true" : "false");
            first = false;
        }
    }
    std::printf("\n  ]\n}\n");
    return 0;
}
