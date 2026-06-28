#![allow(unused)]
use bitcoincore_rpc::bitcoin::{Address, Amount, BlockHash, Txid};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde::Deserialize;
use serde_json::json;
use std::fs::File;
use std::io::Write;

// Connection parameters for the Bitcoin regtest node
const RPC_URL: &str = "http://127.0.0.1:18443";
const RPC_USER: &str = "alice";
const RPC_PASS: &str = "password";

/// A helper function to load an existing wallet or create it if it doesn't exist.
/// Returns a wallet-specific client for RPC commands.
fn get_wallet_rpc(rpc: &Client, name: &str) -> bitcoincore_rpc::Result<Client> {
    // List currently loaded wallets
    let loaded_wallets = rpc.list_wallets()?;
    if !loaded_wallets.contains(&name.to_string()) {
        // Attempt to load the wallet from the datadir
        if let Err(e) = rpc.load_wallet(name) {
            println!("Wallet '{}' could not be loaded ({:?}). Creating a new one instead...", name, e);
            // Create a new descriptor/legacy wallet with private keys enabled
            rpc.create_wallet(name, Some(false), Some(false), None, None)?;
        }
    }
    // Return a client pointing directly to the wallet-specific RPC endpoint
    let wallet_url = format!("{}/wallet/{}", RPC_URL, name);
    Client::new(&wallet_url, Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()))
}

fn main() -> bitcoincore_rpc::Result<()> {
    // 1. Connect to the local Bitcoin Core regtest node
    println!("Connecting to Bitcoin Core RPC at {}...", RPC_URL);
    let rpc = Client::new(
        RPC_URL,
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )?;

    // Log connection success
    println!("Connection successful.");

    // 2. Load or create the 'Miner' and 'Trader' wallets
    println!("Setting up 'Miner' and 'Trader' wallets...");
    let miner_rpc = get_wallet_rpc(&rpc, "Miner")?;
    let trader_rpc = get_wallet_rpc(&rpc, "Trader")?;

    // 3. Generate a new address from the Miner wallet with the label "Mining Reward"
    // We assume the returned address network check is verified since we are running on Regtest.
    let miner_address = miner_rpc.get_new_address(Some("Mining Reward"), None)?.assume_checked();
    println!("Generated Miner address: {}", miner_address);

    // 4. Mine new blocks until the Miner wallet has a positive spendable balance.
    // We observe the balance after each mined block.
    println!("Mining blocks to generate spendable balance...");
    let mut blocks_mined = 0;
    while miner_rpc.get_balance(None, None)? == Amount::ZERO {
        miner_rpc.generate_to_address(1, &miner_address)?;
        blocks_mined += 1;
    }
    println!("Successfully reached a positive balance after mining {} blocks.", blocks_mined);

    /*
     * Why did the Miner's balance stay at zero until we mined 101 blocks?
     * This is due to Bitcoin's "Coinbase Maturity" consensus rule, which locks block rewards
     * (coinbase transactions) for 100 blocks. If a chain split or reorganization (reorg) occurs,
     * those blocks can disappear. Locking rewards ensures that no one spends money that could
     * be reverted if the block gets orphaned. Once block 101 is mined, the block 1 reward finally
     * matures!
     * 
     * Side Note: I bypassed Docker and ran this setup bare metal directly on WSL to verify
     * that it works outside of docker containers, and it did successfully!
     */

    // 5. Print the current balance of the Miner wallet
    let miner_balance = miner_rpc.get_balance(None, None)?;
    println!("Miner Wallet Balance: {} BTC", miner_balance.to_btc());

    // 6. Create a receiving address labeled "Received" from the Trader wallet
    let trader_address = trader_rpc.get_new_address(Some("Received"), None)?.assume_checked();
    println!("Generated Trader receiving address: {}", trader_address);

    // 7. Send a transaction paying 20 BTC from the Miner wallet to the Trader wallet
    let send_amount = Amount::from_btc(20.0)?;
    println!("Sending 20 BTC from Miner to Trader...");
    let txid = miner_rpc.send_to_address(
        &trader_address,
        send_amount,
        None,
        None,
        None,
        None,
        None,
        None,
    )?;
    println!("Transaction sent. Txid: {}", txid);

    // 8. Fetch the unconfirmed transaction from the mempool and print it
    println!("Fetching mempool entry for the unconfirmed transaction...");
    let mempool_entry = miner_rpc.get_mempool_entry(&txid)?;
    println!("Mempool Entry: {:?}", mempool_entry);

    // 9. Confirm the transaction by mining 1 block to the Miner's address
    println!("Mining 1 block to confirm the transaction...");
    miner_rpc.generate_to_address(1, &miner_address)?;

    // 10. Extract all required transaction details for the output report
    println!("Extracting transaction details...");
    // Fetch full transaction receipt from wallet history (includes raw tx hex)
    let tx_info = miner_rpc.get_transaction(&txid, Some(true))?;
    // Decode the raw hex to analyze inputs and outputs
    let decoded_tx = miner_rpc.decode_raw_transaction(&tx_info.hex, None)?;

    // Find the inputs (vins) details
    let vin = &decoded_tx.vin[0];
    let prev_txid = vin.txid.as_ref().expect("Input should have prev txid");
    let prev_vout = vin.vout.expect("Input should have prev vout");
    // Fetch and decode the spent transaction to find its address and value
    let prev_tx_info = miner_rpc.get_transaction(prev_txid, Some(true))?;
    let prev_decoded = miner_rpc.decode_raw_transaction(&prev_tx_info.hex, None)?;
    let prev_output = &prev_decoded.vout[prev_vout as usize];
    
    let miner_input_amount = prev_output.value.to_btc();
    let miner_input_address = prev_output.script_pub_key.address.as_ref()
        .map(|a| a.clone().assume_checked().to_string())
        .or_else(|| {
            prev_output.script_pub_key.addresses.first()
                .map(|a| a.clone().assume_checked().to_string())
        })
        .expect("Previous output should have an address");

    // Scan outputs (vouts) to distinguish between destination output and change output
    let mut trader_output_address = String::new();
    let mut trader_output_amount = 0.0;
    let mut miner_change_address = String::new();
    let mut miner_change_amount = 0.0;

    for output in &decoded_tx.vout {
        let is_trader = if let Some(ref addr) = output.script_pub_key.address {
            addr.clone().assume_checked().to_string() == trader_address.to_string()
        } else {
            output.script_pub_key.addresses.iter()
                .any(|a| a.clone().assume_checked().to_string() == trader_address.to_string())
        };
            
        if is_trader {
            trader_output_address = trader_address.to_string();
            trader_output_amount = output.value.to_btc();
        } else {
            miner_change_address = if let Some(ref addr) = output.script_pub_key.address {
                addr.clone().assume_checked().to_string()
            } else {
                output.script_pub_key.addresses.first()
                    .map(|a| a.clone().assume_checked().to_string())
                    .unwrap_or_default()
            };
            miner_change_amount = output.value.to_btc();
        }
    }

    // Read fee details and confirmation block info
    let fee_btc = tx_info.fee.expect("Transaction should have a fee").to_btc();
    let block_hash = tx_info.info.blockhash.expect("Transaction should be confirmed");
    let block_info = miner_rpc.get_block_info(&block_hash)?;
    let block_height = block_info.height;

    // 11. Write the extracted data to ../out.txt in the exact requested format
    println!("Writing details to out.txt...");
    let mut file = File::create("../out.txt")?;
    writeln!(file, "{}", txid)?;
    writeln!(file, "{}", miner_input_address)?;
    writeln!(file, "{}", miner_input_amount)?;
    writeln!(file, "{}", trader_output_address)?;
    writeln!(file, "{}", trader_output_amount)?;
    writeln!(file, "{}", miner_change_address)?;
    writeln!(file, "{}", miner_change_amount)?;
    writeln!(file, "{}", fee_btc)?;
    writeln!(file, "{}", block_height)?;
    writeln!(file, "{}", block_hash)?;

    println!("All steps completed successfully. out.txt has been generated.");
    Ok(())
}
