#!/usr/bin/env python3
"""Check shared EVM behavior on a local chain using public development keys."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.parse import urlparse
from urllib.request import Request, urlopen

SOURCE = """pragma solidity 0.8.37;
contract ComparisonProbe {
    uint256 public value;
    event Changed(uint256 value);
    function set(uint256 next) external { value = next; emit Changed(next); }
    function fail() external { value = 99; revert("comparison"); }
}
"""
PUBLIC_KEYS = {
    "0x" + "0" * 63 + "1",
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def rpc(url, method, params):
    request = Request(
        url,
        json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }
        ).encode(),
        {"Content-Type": "application/json"},
    )
    with urlopen(request, timeout=10) as response:
        body = json.load(response)
    require("error" not in body, f"{method}: {body.get('error')}")
    return body["result"]


def cast(*arguments):
    return subprocess.check_output(["cast", *arguments], text=True).strip()


def common_receipt(urls, tx_hash, status):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        receipts = [rpc(url, "eth_getTransactionReceipt", [tx_hash]) for url in urls]
        if all(receipts):
            first = receipts[0]
            require(
                all(r["transactionHash"].lower() == tx_hash.lower() for r in receipts),
                "receipt transaction mismatch",
            )
            require(
                all(int(r["status"], 16) == status for r in receipts),
                "unexpected execution status",
            )
            require(
                all(r["blockHash"] == first["blockHash"] for r in receipts),
                "validator receipt disagreement",
            )
            for url in urls:
                finalized = rpc(url, "eth_getBlockByNumber", ["finalized", False])
                if not finalized or int(finalized["number"], 16) < int(
                    first["blockNumber"], 16
                ):
                    break
                block = rpc(url, "eth_getBlockByNumber", [first["blockNumber"], False])
                require(
                    block and block["hash"] == first["blockHash"],
                    "receipt is not canonical",
                )
            else:
                return first
        time.sleep(0.1)
    raise TimeoutError("transaction did not finalize on every selected endpoint")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc", action="append", required=True)
    parser.add_argument("--solc", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument(
        "--verify", type=Path, help="Recheck an existing smoke result after restart"
    )
    args = parser.parse_args()
    for url in args.rpc:
        parsed = urlparse(url)
        require(
            parsed.scheme == "http" and parsed.hostname in {"127.0.0.1", "localhost"},
            "local HTTP RPC endpoints required",
        )
    chains = [rpc(url, "eth_chainId", []) for url in args.rpc]
    require(len(set(chains)) == 1, "chain IDs disagree")
    genesis = [
        rpc(url, "eth_getBlockByNumber", ["0x0", False])["hash"] for url in args.rpc
    ]
    require(len(set(genesis)) == 1, "genesis hashes disagree")
    result = {"chain_id": chains[0], "genesis": genesis[0], "endpoints": args.rpc}
    try:
        if args.verify:
            previous = json.loads(args.verify.read_text())
            require(previous["status"] == "passed", "previous smoke did not pass")
            require(
                previous["chain_id"] == chains[0] and previous["genesis"] == genesis[0],
                "restart changed chain identity",
            )
            contract = previous["contract"]
            for tx in previous["transactions"]:
                common_receipt(args.rpc, tx["hash"], tx["status"])
            for url in args.rpc:
                require(
                    int(rpc(url, "eth_getStorageAt", [contract, "0x0", "latest"]), 16)
                    == 7,
                    "restart lost contract state",
                )
            result.update(
                status="passed",
                contract=contract,
                checks=["restart persistence", "canonical receipts on all endpoints"],
            )
        else:
            require(args.solc and args.solc.is_file(), "--solc is required")
            key = os.environ.get("BENCH_KEY", "").split(",")[0]
            require(
                key in PUBLIC_KEYS,
                "BENCH_KEY must be an allowed public development key",
            )
            sender = cast("wallet", "address", "--private-key", key)
            gas_price = max(
                1_000_000_000, int(rpc(args.rpc[0], "eth_gasPrice", []), 16) * 2
            )
            nonce = int(
                rpc(args.rpc[0], "eth_getTransactionCount", [sender, "pending"]), 16
            )
            transactions = []

            def send(to=None, data="0x", value=0, status=1):
                nonlocal nonce
                options = [
                    "mktx",
                    "--private-key",
                    key,
                    "--chain",
                    str(int(chains[0], 16)),
                    "--nonce",
                    str(nonce),
                    "--gas-price",
                    str(gas_price),
                    "--gas-limit",
                    "1000000",
                    "--legacy",
                ]
                if to:
                    raw = cast(*options, "--value", str(value), to, data)
                else:
                    raw = cast(*options, "--create", data)
                tx_hash = rpc(args.rpc[0], "eth_sendRawTransaction", [raw])
                nonce += 1
                receipt = common_receipt(args.rpc, tx_hash, status)
                transactions.append(
                    {"hash": tx_hash, "status": status, "block": receipt["blockNumber"]}
                )
                return receipt

            recipient = "0x000000000000000000000000000000000000beef"
            balance = int(rpc(args.rpc[0], "eth_getBalance", [recipient, "latest"]), 16)
            send(recipient, value=12345)
            require(
                all(
                    int(rpc(url, "eth_getBalance", [recipient, "latest"]), 16)
                    == balance + 12345
                    for url in args.rpc
                ),
                "transfer balance mismatch",
            )
            compiler_input = {
                "language": "Solidity",
                "sources": {"Probe.sol": {"content": SOURCE}},
                "settings": {
                    "optimizer": {"enabled": True, "runs": 200},
                    "evmVersion": "osaka",
                    "outputSelection": {"*": {"*": ["evm.bytecode.object"]}},
                },
            }
            output = subprocess.check_output(
                [str(args.solc), "--standard-json"],
                input=json.dumps(compiler_input),
                text=True,
            )
            compiled = json.loads(output)
            require(
                not any(e["severity"] == "error" for e in compiled.get("errors", [])),
                "Solidity compilation failed",
            )
            bytecode = (
                "0x"
                + compiled["contracts"]["Probe.sol"]["ComparisonProbe"]["evm"][
                    "bytecode"
                ]["object"]
            )
            contract = send(data=bytecode)["contractAddress"]
            require(contract, "deployment returned no contract address")
            changed = send(contract, cast("calldata", "set(uint256)", "7"))
            require(
                len(changed["logs"]) == 1 and int(changed["logs"][0]["data"], 16) == 7,
                "event mismatch",
            )
            failed = send(contract, cast("calldata", "fail()"), status=0)
            require(not failed["logs"], "reverted transaction retained logs")
            for url in args.rpc:
                require(
                    rpc(url, "eth_getCode", [contract, "latest"]) != "0x",
                    "deployed code missing",
                )
                require(
                    int(rpc(url, "eth_getStorageAt", [contract, "0x0", "latest"]), 16)
                    == 7,
                    "revert failed to roll back storage",
                )
                require(
                    int(
                        rpc(
                            url,
                            "eth_call",
                            [
                                {"to": contract, "data": cast("calldata", "value()")},
                                "latest",
                            ],
                        ),
                        16,
                    )
                    == 7,
                    "contract call mismatch",
                )
                logs = rpc(
                    url,
                    "eth_getLogs",
                    [
                        {
                            "address": contract,
                            "fromBlock": changed["blockNumber"],
                            "toBlock": changed["blockNumber"],
                        }
                    ],
                )
                require(
                    len(logs) == 1
                    and logs[0]["transactionHash"] == changed["transactionHash"],
                    "log query mismatch",
                )
            result.update(
                status="passed",
                contract=contract,
                transactions=transactions,
                checks=[
                    "native transfer",
                    "Solidity deployment",
                    "storage write/read",
                    "revert rollback",
                    "event log",
                    "eth_call",
                    "canonical finalized receipts on all endpoints",
                ],
            )
    except Exception as error:
        result.update(status="failed", error=str(error))
        raise
    finally:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
