# kithara-analysis-tests

Integration tests for the public `kithara::analysis` facade live in
[`tests`](tests). The package isolates realtime analysis contracts in their own
test binary so unrelated test changes can reuse its compiled artifact.
