// Core's UniValue float construction (UniValue::setFloat), for src/univalue.rs's tests: prints
// each value's JSON text or the error setNumStr throws, and the input's sign bit. Build against
// the tagged Core sources (src/univalue) with MinGW-w64 g++ and run on Windows or Wine:
//   U=<core>/src/univalue
//   x86_64-w64-mingw32-g++-posix -std=c++17 -O2 -static -I$U/include -I$U/lib \
//       univalue_float_probe.cpp $U/lib/univalue*.cpp -o probe.exe
#include <univalue.h>
#include <cmath>
#include <iostream>
#include <limits>
int main() {
    const double values[] = {3.25, 0.1, 1e20, 1.5e-5, 123456789012345678.0, 100.0,
        std::numeric_limits<double>::max(), -0.0, 5e-324,
        std::numeric_limits<double>::infinity(), -std::numeric_limits<double>::infinity(),
        std::numeric_limits<double>::quiet_NaN(), -std::numeric_limits<double>::quiet_NaN()};
    for (double v : values) {
        try { std::cout << "ok\t" << UniValue(v).write() << "\n"; }
        catch (const std::exception& e) { std::cout << "err\t" << e.what() << "\t" << std::signbit(v) << "\n"; }
    }
}
