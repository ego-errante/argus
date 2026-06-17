# /// script
# requires-python = ">=3.10"
# dependencies = ["solders>=0.21", "requests>=2.31"]
# ///
"""Throwaway probe: does Helius Sender land a real mainnet tx for us?

Mirrors the Rust tracer bundle as ONE transaction (priority fee + memo nonce +
tip), submits to Helius Sender's keyless /fast endpoint (dual-routes staked+Jito),
then confirms via the working SolInfra RPC. Not wired into argus-core — pure test.

Run from the repo root:  uv run scripts/helius_sender_test.py
"""
import base64
import json
import re
import struct
import sys
import time
from pathlib import Path

import requests
from solders.hash import Hash
from solders.instruction import Instruction
from solders.keypair import Keypair
from solders.message import Message
from solders.pubkey import Pubkey
from solders.system_program import TransferParams, transfer
from solders.transaction import Transaction

# ---- tunables -------------------------------------------------------------
SWQOS_ONLY = False          # False = dual-route (staked + Jito); True = staked-only, cheaper
DUAL_TIP_LAMPORTS = 200_000  # 0.0002 SOL (Sender dual-route minimum)
SWQOS_TIP_LAMPORTS = 5_000   # 0.000005 SOL (Sender swqos_only minimum)
CU_LIMIT = 20_000
CU_PRICE_MICROLAMPORTS = 100_000  # priority fee; ~2000 lamports at the limit above
CONFIRM_TRIES = 20
CONFIRM_DELAY_S = 2.0

SENDER_URL = "https://sender.helius-rpc.com/fast" + ("?swqos_only=true" if SWQOS_ONLY else "")
MEMO_PROGRAM = Pubkey.from_string("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr")
COMPUTE_BUDGET = Pubkey.from_string("ComputeBudget111111111111111111111111111111")
# Helius Sender mainnet tip accounts (any one is fine).
HELIUS_TIP_ACCOUNT = Pubkey.from_string("4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE")

REPO = Path(__file__).resolve().parent.parent


def read_env(key: str) -> str:
    text = (REPO / ".env").read_text()
    m = re.search(rf"^{re.escape(key)}=(.*)$", text, re.MULTILINE)
    if not m or not m.group(1).strip():
        sys.exit(f"missing {key} in .env")
    return m.group(1).strip()


def load_keypair() -> Keypair:
    arr = json.loads((REPO / "secrets" / "keypair.json").read_text())
    return Keypair.from_bytes(bytes(arr))


def cu_limit_ix(units: int) -> Instruction:
    return Instruction(COMPUTE_BUDGET, bytes([0x02]) + struct.pack("<I", units), [])


def cu_price_ix(micro_lamports: int) -> Instruction:
    return Instruction(COMPUTE_BUDGET, bytes([0x03]) + struct.pack("<Q", micro_lamports), [])


def get_blockhash(rpc: str) -> Hash:
    body = {"jsonrpc": "2.0", "id": 1, "method": "getLatestBlockhash",
            "params": [{"commitment": "confirmed"}]}
    r = requests.post(rpc, json=body, timeout=15).json()
    return Hash.from_string(r["result"]["value"]["blockhash"])


def confirm(rpc: str, sig: str) -> int | None:
    body = {"jsonrpc": "2.0", "id": 1, "method": "getSignatureStatuses",
            "params": [[sig], {"searchTransactionHistory": True}]}
    for _ in range(CONFIRM_TRIES):
        v = requests.post(rpc, json=body, timeout=15).json()["result"]["value"][0]
        if v and v.get("confirmationStatus") in ("confirmed", "finalized"):
            return v.get("slot")
        time.sleep(CONFIRM_DELAY_S)
    return None


def main() -> None:
    rpc = read_env("RPC_HTTP_URL")
    payer = load_keypair()
    tip = SWQOS_TIP_LAMPORTS if SWQOS_ONLY else DUAL_TIP_LAMPORTS
    nonce = f"argus-helius-test-{int(time.time())}"
    print(f"payer       : {payer.pubkey()}")
    print(f"route       : {'swqos_only' if SWQOS_ONLY else 'dual (staked + Jito)'}")
    print(f"tip lamports: {tip}   nonce: {nonce}")

    blockhash = get_blockhash(rpc)
    ixs = [
        cu_limit_ix(CU_LIMIT),
        cu_price_ix(CU_PRICE_MICROLAMPORTS),
        Instruction(MEMO_PROGRAM, nonce.encode(), []),
        transfer(TransferParams(from_pubkey=payer.pubkey(),
                                to_pubkey=HELIUS_TIP_ACCOUNT, lamports=tip)),
    ]
    msg = Message.new_with_blockhash(ixs, payer.pubkey(), blockhash)
    tx = Transaction([payer], msg, blockhash)
    sig = str(tx.signatures[0])
    b64 = base64.b64encode(bytes(tx)).decode()
    explorer = f"https://solscan.io/tx/{sig}"
    print(f"signature   : {sig}\nexplorer    : {explorer}")

    body = {"jsonrpc": "2.0", "id": nonce, "method": "sendTransaction",
            "params": [b64, {"encoding": "base64", "skipPreflight": True, "maxRetries": 0}]}
    resp = requests.post(SENDER_URL, json=body, timeout=20).json()
    print(f"sender resp : {resp}")
    if "error" in resp:
        sys.exit("Sender rejected the submission ^")

    print("submitted — confirming via SolInfra RPC ...")
    slot = confirm(rpc, sig)
    if slot:
        print(f"\n✅ LANDED in slot {slot}\n{explorer}")
    else:
        print(f"\n❌ not confirmed within "
              f"{int(CONFIRM_TRIES * CONFIRM_DELAY_S)}s (no fee if it never landed)\n{explorer}")


if __name__ == "__main__":
    main()
