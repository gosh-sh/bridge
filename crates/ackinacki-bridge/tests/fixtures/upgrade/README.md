# Legacy upgrade fixture

`eccUSDCBridge-1.15.0.tvc` is the sold 0.80.0 production artifact from
commit `6f53305f` of `main`, before the AN token registry.
Its code hash begins `71074c89`. The upgrade test deploys it, credits USDC,
and invokes its original `updateCode` against the current production artifact.
The matching ABI is retained because SDK deployment initializes storage from
its field layout. The constructor, owner-mint and upgrade call signatures
are unchanged.
