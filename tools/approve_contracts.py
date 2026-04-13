"""
tools/approve_contracts.py
──────────────────────────
Grant MAX_INT USDC allowance to Polymarket's CTF Exchange and NegRisk CTF
contracts on Polygon PoS (chain ID 137).

Usage:
    python tools/approve_contracts.py

Requirements:
    pip install web3==7.12.1 python-dotenv
"""

from __future__ import annotations

import os
import sys

from dotenv import load_dotenv
from web3 import Web3
from web3.middleware import ExtraDataToPOAMiddleware

# ── Constants ─────────────────────────────────────────────────────────────────

CHAIN_ID = 137
RPC_URL = "https://polygon-rpc.com"

USDC_ADDRESS       = Web3.to_checksum_address("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174")
CTF_EXCHANGE       = Web3.to_checksum_address("0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E")
NEG_RISK_CTF       = Web3.to_checksum_address("0xC5d563A36AE78145C45a50134d48A1215220f80a")

MAX_INT = 2**256 - 1   # standard ERC-20 unlimited approval

# Minimal ERC-20 ABI (only the calls we need)
ERC20_ABI = [
    {
        "name": "approve",
        "type": "function",
        "inputs": [
            {"name": "spender", "type": "address"},
            {"name": "amount",  "type": "uint256"},
        ],
        "outputs": [{"name": "", "type": "bool"}],
        "stateMutability": "nonpayable",
    },
    {
        "name": "allowance",
        "type": "function",
        "inputs": [
            {"name": "owner",   "type": "address"},
            {"name": "spender", "type": "address"},
        ],
        "outputs": [{"name": "", "type": "uint256"}],
        "stateMutability": "view",
    },
    {
        "name": "symbol",
        "type": "function",
        "inputs": [],
        "outputs": [{"name": "", "type": "string"}],
        "stateMutability": "view",
    },
]


# ── Helpers ───────────────────────────────────────────────────────────────────


def _load_private_key() -> str:
    """Load POLYGON_PRIVATE_KEY from .env or prompt interactively."""
    load_dotenv()
    key = os.environ.get("POLYGON_PRIVATE_KEY", "").strip()
    if not key:
        key = input("POLYGON_PRIVATE_KEY not found in .env.  Enter it now: ").strip()
    if not key:
        print("[Error] No private key provided — exiting.", file=sys.stderr)
        sys.exit(1)
    return key


def _connect() -> Web3:
    w3 = Web3(Web3.HTTPProvider(RPC_URL))
    # Polygon uses PoA consensus; this middleware drops the extraData size limit
    w3.middleware_onion.inject(ExtraDataToPOAMiddleware, layer=0)
    if not w3.is_connected():
        print(f"[Error] Cannot connect to Polygon RPC: {RPC_URL}", file=sys.stderr)
        sys.exit(1)
    return w3


def _current_allowance(usdc: object, owner: str, spender: str) -> int:
    return usdc.functions.allowance(owner, spender).call()  # type: ignore[union-attr]


def _already_approved(allowance: int) -> bool:
    # Consider it already approved if allowance > half of MAX_INT
    return allowance > MAX_INT // 2


def _send_approval(
    w3: Web3,
    usdc: object,
    spender: str,
    spender_label: str,
    account: object,
) -> str:
    """Build, sign, and broadcast a single approve() transaction. Returns tx hash."""
    nonce = w3.eth.get_transaction_count(account.address)
    gas_price = w3.eth.gas_price

    txn = usdc.functions.approve(spender, MAX_INT).build_transaction(  # type: ignore[union-attr]
        {
            "chainId": CHAIN_ID,
            "from": account.address,
            "nonce": nonce,
            "gasPrice": gas_price,
        }
    )
    # Estimate gas with a 20 % buffer
    estimated = w3.eth.estimate_gas(txn)
    txn["gas"] = int(estimated * 1.2)

    signed = account.sign_transaction(txn)
    tx_hash = w3.eth.send_raw_transaction(signed.raw_transaction)
    receipt = w3.eth.wait_for_transaction_receipt(tx_hash, timeout=120)

    status = "✓ confirmed" if receipt.status == 1 else "✗ reverted"
    print(f"  {status}  tx={tx_hash.hex()}  gas_used={receipt.gasUsed}")
    if receipt.status != 1:
        sys.exit(1)
    return tx_hash.hex()


# ── Main ──────────────────────────────────────────────────────────────────────


def main() -> None:
    private_key = _load_private_key()
    w3 = _connect()

    account = w3.eth.account.from_key(private_key)
    wallet  = account.address

    usdc     = w3.eth.contract(address=USDC_ADDRESS, abi=ERC20_ABI)
    symbol   = usdc.functions.symbol().call()
    bal_wei  = w3.eth.get_balance(wallet)
    bal_matic = w3.from_wei(bal_wei, "ether")

    # ── Pre-flight display ────────────────────────────────────────────────────
    print()
    print("=" * 60)
    print("  Polymarket Contract Approval — Polygon PoS")
    print("=" * 60)
    print(f"  Wallet         : {wallet}")
    print(f"  MATIC balance  : {bal_matic:.4f} MATIC  (needed for gas)")
    print(f"  Token          : {symbol}  ({USDC_ADDRESS})")
    print()
    print("  Spenders to approve:")
    print(f"    [1] CTF Exchange  {CTF_EXCHANGE}")
    print(f"    [2] NegRisk CTF   {NEG_RISK_CTF}")
    print(f"  Amount         : MAX_INT (unlimited)")
    print("=" * 60)

    if bal_matic < 0.01:
        print(
            f"\n[Warning] Low MATIC balance ({bal_matic:.4f}).  "
            "You need MATIC to pay gas on Polygon.",
            file=sys.stderr,
        )

    # ── Confirmation gate ─────────────────────────────────────────────────────
    print()
    answer = input("Type 'yes' to proceed, anything else to abort: ").strip().lower()
    if answer != "yes":
        print("Aborted — no transactions sent.")
        sys.exit(0)

    # ── Check existing allowances ─────────────────────────────────────────────
    approvals = [
        (CTF_EXCHANGE, "CTF Exchange"),
        (NEG_RISK_CTF, "NegRisk CTF"),
    ]

    print()
    for spender, label in approvals:
        allowance = _current_allowance(usdc, wallet, spender)
        if _already_approved(allowance):
            print(f"  [skip] {label}: already approved (allowance={allowance})")
            continue

        print(f"  [send] Approving {label} ...")
        _send_approval(w3, usdc, spender, label, account)

    print()
    print("Done.  Both spenders have MAX_INT USDC allowance.")
    print("You can now run the engine in live mode:")
    print("  cargo run --release -- --mode live --confirm-live")


if __name__ == "__main__":
    main()
