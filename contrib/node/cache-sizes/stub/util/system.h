// A stand-in for Core's ArgsManager (util/system.h) with only what node/caches.cpp and probe.cpp
// call, so that gen.py can build node/caches.cpp without a Core build tree (for the 32-bit grid).
// The probe's values are decimal integers, which GetIntArg and GetBoolArg read as Core's do
// (LocaleIndependentAtoi, InterpretBool); gen.py checks that a 64-bit build of the stub probe
// reproduces the full build's grid.
#pragma once

#include <cstdint>
#include <map>
#include <string>

class ArgsManager
{
    std::map<std::string, std::string> m_values;

public:
    void ForceSetArg(const std::string& name, const std::string& value) { m_values[name] = value; }

    int64_t GetIntArg(const std::string& name, int64_t default_value) const
    {
        auto it = m_values.find(name);
        return it == m_values.end() ? default_value : std::stoll(it->second);
    }

    bool GetBoolArg(const std::string& name, bool default_value) const
    {
        return GetIntArg(name, default_value) != 0;
    }
};
