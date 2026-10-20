import importlib
import os
import unittest
from unittest.mock import patch

from helper import bridge_e2e as bridge


class WithdrawalChainLimits(unittest.TestCase):
    def test_default_and_configured_destination(self):
        try:
            with patch.dict(os.environ, {}, clear=True):
                self.assertEqual(importlib.reload(bridge).DST_CHAIN_ID, 11155111)
            with patch.dict(os.environ, {"DST_CHAIN_ID": "42161"}):
                self.assertEqual(importlib.reload(bridge).DST_CHAIN_ID, 42161)
        finally:
            importlib.reload(bridge)

    def withdraw(self, chain=11155111):
        return bridge.call_initiate_withdrawal("wallet", "wallet.abi", "keys", chain, "11" * 20)

    def test_only_selected_chains_inventory_can_cover_withdrawal(self):
        def getter(address, abi, name, params):
            if name == "getDepositLimits":
                return {"maxTransfer": "50000000", "outstanding": "1000000" if params["chainId"] == "11155111" else "0"}
            return {"minted": "1000000", "burned": "0"}

        with patch.object(bridge.common, "run_getter", side_effect=getter), \
                patch.object(bridge, "encode_initiate_withdrawal_body", return_value="body") as encode, \
                patch.object(bridge.common, "call_contract", return_value="sent") as send:
            with self.assertRaisesRegex(RuntimeError, r"getDepositLimits\(1\)"):
                self.withdraw(1)
            encode.assert_not_called()
            send.assert_not_called()
            self.assertEqual(self.withdraw(), "sent")
            encode.assert_called_once_with(11155111, "11" * 20)
            self.assertEqual(send.call_args.args[4]["cc"], {"3": "1000000"})

    def test_oversized_and_unavailable_limits_stop_before_ecc_transfer(self):
        for payload in (None, {}, {"maxTransfer": "bad", "outstanding": "1000000"},
                        {"maxTransfer": "999999", "outstanding": "1000000"}):
            with self.subTest(payload=payload), \
                    patch.object(bridge.common, "run_getter", return_value=payload), \
                    patch.object(bridge, "encode_initiate_withdrawal_body") as encode, \
                    patch.object(bridge.common, "call_contract") as send:
                with self.assertRaises(RuntimeError):
                    self.withdraw()
                encode.assert_not_called()
                send.assert_not_called()

    def test_rpc_failure_stops_before_ecc_transfer(self):
        with patch.object(bridge.common, "run_getter", side_effect=RuntimeError("RPC unavailable")), \
                patch.object(bridge.common, "call_contract") as send:
            with self.assertRaisesRegex(RuntimeError, "RPC unavailable"):
                self.withdraw()
            send.assert_not_called()


if __name__ == "__main__":
    unittest.main()
