// The libstdc++ std::filesystem results dash-args' Windows path tests expect (src/paths.rs):
// root names, is_absolute and operator/ for fixed inputs, then absolute() of each argument
// relative to the current directory. Build with MinGW-w64 g++ and run on Windows or Wine:
//   x86_64-w64-mingw32-g++ -std=c++17 -static -o probe.exe windows_paths_probe.cpp
//   probe.exe x Z:x "\\x" ...
#include <filesystem>
#include <iostream>
#include <string>
#include <vector>
namespace fs = std::filesystem;
int main(int argc, char** argv) {
    std::vector<std::string> paths = {
        "C:", "C:x", "C:\\", "C:\\x", "C:/x", "\\x", "/x", "\\\\x", "x",
        "\\\\server", "\\\\server\\share", "//server/share/x", "\\\\\\x",
        "\\\\?\\C:\\x", "\\\\?\\UNC\\server\\share", "1:x", "", "\\\\.\\C:\\x", "/:x", "\\:x",
    };
    for (auto& p : paths) {
        fs::path q{p};
        std::cout << "P\t" << p << "\t" << q.root_name().string() << "\t" << q.is_absolute() << "\n";
    }
    std::vector<std::pair<std::string, std::string>> joins = {
        {"C:\\d", "x"}, {"C:\\d\\", "x"}, {"C:\\d", "C:x"}, {"C:\\d", "D:x"},
        {"C:\\d", "\\x"}, {"C:\\d", "C:\\x"}, {"C:\\d", "D:\\x"}, {"C:\\d", "c:x"},
        {"C:", "x"}, {"C:\\", "x"}, {"C:/d", "x"}, {"\\\\server\\share", "x"},
        {"\\\\server\\share", "\\x"}, {"\\\\server\\share", "C:x"}, {"\\\\server", "x"},
        {"\\\\?\\C:\\d", "x"}, {"\\\\?\\C:\\d", "C:x"}, {"C:\\d", "\\\\?\\C:\\x"},
        {"C:\\d", "\\\\server\\x"}, {"C:\\d", "//server"}, {"", "x"}, {"C:\\d", "x/y\\z"},
        {"C:\\d", "/x"}, {"C:\\d", "C:"}, {"C:\\d", "\\\\\\x"}, {"C:\\d", "\\:x"},
    };
    for (auto& [a, b] : joins) {
        std::cout << "J\t" << a << "\t" << b << "\t" << (fs::path{a} / fs::path{b}).string() << "\n";
    }
    std::cout << "C\t" << fs::current_path().string() << "\n";
    for (int i = 1; i < argc; ++i) {
        std::cout << "A\t" << argv[i] << "\t" << fs::absolute(argv[i]).string() << "\n";
    }
}
