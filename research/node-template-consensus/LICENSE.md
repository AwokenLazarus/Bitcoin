# License

Copyright (c) 2026 Mike Moore (AwokenLazarus)

Everything in this folder is by Mike Moore (AwokenLazarus) unless a file says otherwise.

| What | Licence |
|---|---|
| Documents: every `.md` file, `index.html` (rendered from the BIP) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| Code: the Python model, `vectors/*.py`, `generate_html.py` | MIT, full text below |
| Data: `vectors/nta-vectors.json` | MIT, as the code that reads it |
| `block_header_v2_vector.json` | one vector from Bitcoin Knots' `src/test/data/block_header_v2.json` (its `source` field says which): MIT, Copyright (c) The Bitcoin Core developers |
| [`patches/knots-v29.4.2/`](patches/knots-v29.4.2/) | MIT, the licence of Bitcoin Knots. The upstream copyright notices in the patched files are kept; see its README |

Code files carry an `SPDX-License-Identifier` header. `xbt_pow.py` ports
`CBlockHeader::GetHash` from Bitcoin Knots (Copyright (c) The Bitcoin Core developers, MIT).

## Attribution (CC BY 4.0)

When you reuse or adapt the documents, credit them like this:

> Node Template Attestation (XBT-NTA) by Mike Moore (AwokenLazarus),
> https://github.com/AwokenLazarus/Bitcoin/tree/main/research/node-template-consensus,
> licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/).

Say if you changed anything. The full licence is at
[creativecommons.org/licenses/by/4.0/legalcode](https://creativecommons.org/licenses/by/4.0/legalcode).

## MIT (code)

```
MIT License

Copyright (c) 2026 Mike Moore (AwokenLazarus)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
