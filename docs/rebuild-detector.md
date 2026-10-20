# Rebuild detector

Ordinary successful recordings save optional version 1 `rebuild-evidence.json` before execution and finalize it after normal analysis. Older captures and launcher batches may lack this artifact. Interrupted recovery preserves saved boundaries and does not inspect the later filesystem.

A candidate requires a complete matching SHA256, equal size, different known modification times, and executed dependent actions. Comparisons identify either the previous-build boundary or the during-build boundary. The predecessor is the newest local capture in the same build tree, with successful status, matching mode, configuration, targets, machine, and compiler environment. An incompatible intervening capture prevents predecessor attribution.

Generated-output churn additionally requires a successful observed generator of that path that completed before an associated dependent action. Ninja input explanations are distinguished from dependency association alone. Candidate costs overlap: combined work counts the union of actions once. These values describe work, not predicted savings.

The report retains all observed changes and the complete pre-build snapshot. Missing, oversized, or unstable hashes remain unknown. Partial inventories, failures, profiles, and launcher captures cannot assert detector conclusions. Matching boundary contents cannot prove contents remained unchanged throughout the build, or establish timestamp-dependent macro safety. Other changed inputs and commands may require the same actions.
