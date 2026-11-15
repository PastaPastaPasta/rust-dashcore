// Core's own getblocktxn read (blockencodings.h, compiled from a tag's
// sources): one hex payload per line in, one JSON line out with the bytes
// Core's handler stream had consumed and the exception, if any.
#include <blockencodings.h>
#include <streams.h>
#include <version.h>
#include <util/strencodings.h>
#include <iostream>
#include <string>

int main() {
    std::string line;
    while (std::getline(std::cin, line)) {
        std::vector<unsigned char> bytes = ParseHex(line);
        CDataStream s(SER_NETWORK, PROTOCOL_VERSION);
        s.write(MakeByteSpan(bytes));
        BlockTransactionsRequest req;
        std::string error;
        try {
            s >> req;
        } catch (const std::exception& e) {
            error = e.what();
        }
        std::cout << "{\"payload\":\"" << line << "\",\"consumed\":" << bytes.size() - s.size()
                  << ",\"error\":\"" << error << "\"}" << std::endl;
    }
}
