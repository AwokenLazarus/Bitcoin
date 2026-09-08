# Node Template Attestation

Draft **soft fork**: a coinbase may only pay scripts that brought a
node-template attestation in the same block, and that block must include
those nodes' feasible inclusion-list transactions.

| | |
| --- | --- |
| Spec | [bip-node-template-attestation.md](bip-node-template-attestation.md) |
| Model | `spec.py`, `validate.py` |
| Tests | `python3 test_consensus.py && python3 test_world.py` |

This is research. It is not a Core/Knots patch and has no activation bit.

Regenerate the HTML with `python3 generate_html.py`.
