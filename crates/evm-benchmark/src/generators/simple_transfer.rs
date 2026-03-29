use crate::generators::TxGenerator;
use alloy_network::TransactionBuilder;
use alloy_primitives::Address;
use alloy_primitives::U256;
use alloy_rpc_types::TransactionRequest;

pub struct SimpleTransferGenerator {
    recipient: Address,
    value: U256,
    counter: u32,
}

impl SimpleTransferGenerator {
    pub fn new(recipient: Address, value: U256) -> Self {
        SimpleTransferGenerator {
            recipient,
            value,
            counter: 0,
        }
    }
}

impl TxGenerator for SimpleTransferGenerator {
    fn next(&mut self) -> TransactionRequest {
        self.counter += 1;
        TransactionRequest::default()
            .with_to(self.recipient)
            .with_value(self.value)
            .with_gas_limit(21_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_transfer_generation() {
        use alloy_primitives::TxKind;

        let recipient = Address::default();
        let mut generator = SimpleTransferGenerator::new(recipient, U256::from(1u32));

        let tx1 = generator.next();
        assert_eq!(tx1.to, Some(TxKind::Call(recipient)));
        assert_eq!(tx1.value, Some(U256::from(1u32)));

        let tx2 = generator.next();
        assert_eq!(tx2.to, Some(TxKind::Call(recipient)));
        assert_eq!(generator.counter, 2);
    }

    #[test]
    fn test_generate_100_transfers_counter_increments() {
        let recipient = Address::with_last_byte(0x01);
        let mut generator = SimpleTransferGenerator::new(recipient, U256::from(1u32));

        for _ in 0..100 {
            let _ = generator.next();
        }
        assert_eq!(generator.counter, 100);
    }

    #[test]
    fn test_gas_limit_is_21000() {
        let recipient = Address::with_last_byte(0x01);
        let mut generator = SimpleTransferGenerator::new(recipient, U256::from(1u32));
        let tx = generator.next();
        assert_eq!(tx.gas, Some(21_000));
    }
}
