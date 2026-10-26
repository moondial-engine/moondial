# Security

Moondial is pre-alpha and not yet deployed anywhere that holds other people's data. Even so, the design has two security boundaries that matter from the first commit: the module sandbox (a module gets nothing it did not declare) and the CLEAR gate (no uninspectable channel crosses it).

If you find a way for a module to reach state, commands, events or clocks it did not declare, for a query to return a field its actor may not see, or for a CLEAR message to reach the driver without passing the checker, please report it privately to hello@moondial.ai rather than opening an issue. Include a minimal reproduction. You will get an acknowledgement within a few days and credit in the fix unless you prefer otherwise.

Reports about the websites (agentscanplay.com, moondial.ai) go to the same address.
