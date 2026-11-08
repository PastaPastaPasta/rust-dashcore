# Mainnet anchor-pack slices (SB4 A-10)

Each `mainnet-<h>.bin` holds the 26 mainnet headers at heights `h − 24 ..= h + 1`, 80 bytes
each in height order: an anchor pack for an anchor at `h`, and the header after it. They are
byte ranges of H-M (`../headers/mainnet.bin`, SHA-256
`58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251`, a Dash Core v24 reference
node's dump; `../headers/README.md`). The anchor header was checked against the mainnet
reference node (v24.0.0-rc.3, `docs/node-program/ORACLE.md`) with read-only
`getblockheader <hash> false`, and its absolute chain work is that node's
`getblockheader <hash>` `chainwork`:

| file | anchor hash | chainwork | SHA-256 |
|---|---|---|---|
| `mainnet-34164.bin` | `000000000062d5da23854a141c144147960aa228c5e21c9104e069f34219624f` | `0x39a0741ece39d9` | `e79a0e1811918a15ce8dcce3004d68c62e3d3578ab6ba28a4784012f9f25f898` |
| `mainnet-1500000.bin` | `0000000000000002cb2f0d030e9744cd6d93b913b80958b232e4f0b84d828ecf` | `0x5cc3c1ba9c4e8e7b2a9c` | `078580fe40cd044a556403cea1808db17732eba51b4cfb91f0d7ad19e78139b8` |

To rebuild one: `dd if=mainnet.bin bs=80 skip=$((h - 24)) count=26 of=mainnet-$h.bin`.
