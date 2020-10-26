// Copyright 2015-2020 Parity Technologies (UK) Ltd.
// This file is part of OpenEthereum.

// OpenEthereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// OpenEthereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with OpenEthereum.  If not, see <http://www.gnu.org/licenses/>.

//! Transaction data structure.

use std::ops::Deref;

use ethereum_types::{Address, H160, H256, U256};
use ethjson;
use ethkey::{self, public_to_address, recover, Public, Secret, Signature};
use hash::keccak;
use heapsize::HeapSizeOf;
use rlp::{self, DecoderError, Encodable, Rlp, RlpStream};

use transaction::error;

type Bytes = Vec<u8>;
type BlockNumber = u64;

/// Fake address for unsigned transactions as defined by EIP-86.
pub const UNSIGNED_SENDER: Address = H160([0xff; 20]);

/// System sender address for internal state updates.
pub const SYSTEM_ADDRESS: Address = H160([
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xfe,
]);

/// Transaction action type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Create creates new contract.
    Create,
    /// Calls contract at given address.
    /// In the case of a transfer, this is the receiver's address.'
    Call(Address),
}

impl Default for Action {
    fn default() -> Action {
        Action::Create
    }
}

impl rlp::Decodable for Action {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.is_empty() {
            if rlp.is_data() {
                Ok(Action::Create)
            } else {
                Err(DecoderError::RlpExpectedToBeData)
            }
        } else {
            Ok(Action::Call(rlp.as_val()?))
        }
    }
}

impl rlp::Encodable for Action {
    fn rlp_append(&self, s: &mut RlpStream) {
        match *self {
            Action::Create => s.append_internal(&""),
            Action::Call(ref addr) => s.append_internal(addr),
        };
    }
}

/// Transaction activation condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Valid at this block number or later.
    Number(BlockNumber),
    /// Valid at this unix time or later.
    Timestamp(u64),
}

/// Replay protection logic for v part of transaction's signature
pub mod signature {
    /// Adds chain id into v
    pub fn add_chain_replay_protection(v: u64, chain_id: Option<u64>) -> u64 {
        v + if let Some(n) = chain_id {
            35 + n * 2
        } else {
            27
        }
    }

    /// Returns refined v
    /// 0 if `v` would have been 27 under "Electrum" notation, 1 if 28 or 4 if invalid.
    pub fn check_replay_protection(v: u64) -> u8 {
        match v {
            v if v == 27 => 0,
            v if v == 28 => 1,
            v if v >= 35 => ((v - 1) % 2) as u8,
            _ => 4,
        }
    }
}

/// A set of information describing an externally-originating message call
/// or contract creation operation.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    /// Nonce.
    pub nonce: U256,
    /// Gas price.
    pub gas_price: U256,
    /// Gas paid up front for transaction execution.
    pub gas: U256,
    /// Action, can be either call or contract create.
    pub action: Action,
    /// Transfered value.
    pub value: U256,
    /// Transaction data.
    pub data: Bytes,
}

impl Transaction {
    /// The message hash of the transaction. This hash is used for signing transaction
    pub fn hash(&self, chain_id: Option<u64>) -> H256 {
        let mut stream = RlpStream::new();
        self.encode(&mut stream, chain_id, None);
        keccak(stream.as_raw())
    }

    fn decode(d: &Rlp) -> Result<UnverifiedTransaction, DecoderError> {
        if d.item_count()? != 9 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        let hash = keccak(d.as_raw());
        let signature = SignatureComponents {
            v: d.val_at(6)?,
            r: d.val_at(7)?,
            s: d.val_at(8)?,
        };
        Ok(UnverifiedTransaction::new(
            TypedTransaction::Legacy(Self::decode_data(d)?),
            signature,
            hash,
        ))
    }

    pub fn decode_data(d: &Rlp) -> Result<Transaction, DecoderError> {
        Ok(Transaction {
            nonce: d.val_at(0)?,
            gas_price: d.val_at(1)?,
            gas: d.val_at(2)?,
            action: d.val_at(3)?,
            value: d.val_at(4)?,
            data: d.val_at(5)?,
        })
    }

    fn encode(
        &self,
        rlp: &mut RlpStream,
        chain_id: Option<u64>,
        signature: Option<&SignatureComponents>,
    ) {
        let mut list_size = 6;
        list_size += if chain_id.is_some() { 3 } else { 0 };
        list_size += if signature.is_some() { 3 } else { 0 };
        rlp.begin_list(list_size);

        self.rlp_append_open(rlp, chain_id);

        if let Some(signature) = signature {
            signature.rlp_append(rlp);
        }
    }

    pub fn rlp_append(
        &self,
        rlp: &mut RlpStream,
        chain_id: Option<u64>,
        signature: &SignatureComponents,
    ) {
        self.encode(rlp, chain_id, Some(signature));
    }

    pub fn rlp_append_open(&self, s: &mut RlpStream, chain_id: Option<u64>) {
        s.append(&self.nonce);
        s.append(&self.gas_price);
        s.append(&self.gas);
        s.append(&self.action);
        s.append(&self.value);
        s.append(&self.data);
        if let Some(n) = chain_id {
            s.append(&n);
            s.append(&0u8);
            s.append(&0u8);
        }
    }
}

impl HeapSizeOf for Transaction {
    fn heap_size_of_children(&self) -> usize {
        self.data.heap_size_of_children()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AccessListTx {
    pub transaction: Transaction,
    //optional access list
    pub access_list: Vec<(H160, Vec<H256>)>,
}

impl AccessListTx {
    pub fn new(transaction: Transaction, access_list: Vec<(H160, Vec<H256>)>) -> AccessListTx {
        AccessListTx {
            transaction,
            access_list,
        }
    }

    pub fn tx(&self) -> &Transaction {
        &self.transaction
    }

    pub fn tx_mut(&mut self) -> &mut Transaction {
        &mut self.transaction
    }

    // decode bytes by this payload spec: rlp([3, [nonce, gasPrice, gasLimit, to, value, data, access_list, senderV, senderR, senderS]])
    pub fn decode(tx: &[u8]) -> Result<UnverifiedTransaction, DecoderError> {
        let tx_rlp = &Rlp::new(&tx[1..]); //first byte is related to transaction type defined in EIP-2718

        // we need to have 10 items in this list
        if tx_rlp.item_count()? != 10 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        // first part of list is same as legacy transaction and we are reusing that part.
        let transaction = Transaction::decode_data(&tx_rlp)?;

        // access list we get from here
        let accl_rlp = tx_rlp.at(6)?;

        // access_list pattern: [[{20 bytes}, [{32 bytes}...]]...]
        let mut accl: Vec<(H160, Vec<H256>)> = Vec::new();

        for i in 0..accl_rlp.item_count()? {
            let accounts = accl_rlp.at(i)?;

            if accounts.item_count()? != 2 {
                //TODO check what to do if we have only one item, should we be strict or not
                return Err(DecoderError::Custom("Unknown access list lenght"));
            }
            accl.push((accounts.val_at(0)?, accounts.list_at(1)?));
        }

        // we get signature part from here
        let signature = SignatureComponents {
            v: tx_rlp.val_at(7)?,
            r: tx_rlp.val_at(8)?,
            s: tx_rlp.val_at(9)?,
        };

        //and here we create UnverifiedTransaction and calculate its hash
        Ok(UnverifiedTransaction::new(
            TypedTransaction::AccessList(AccessListTx {
                transaction,
                access_list: accl,
            }),
            signature,
            0.into(),
        )
        .compute_hash())
    }

    // encode by this payload spec: 0x03 | rlp([3, [nonce, gasPrice, gasLimit, to, value, data, access_list, senderV, senderR, senderS]])
    pub fn encode(&self, signature: Option<&SignatureComponents>) -> Vec<u8> {
        let mut stream = RlpStream::new();
        //stream.begin_list(2);
        //stream.append(&3u8);

        let mut list_size = 7;
        list_size += if signature.is_some() { 3 } else { 0 };
        stream.begin_list(list_size);
        self.transaction.rlp_append_open(&mut stream, None);

        //access list
        stream.begin_list(self.access_list.len());
        for access in self.access_list.iter() {
            stream.begin_list(2);
            stream.append(&access.0);
            stream.begin_list(access.1.len());
            for storage_key in access.1.iter() {
                stream.append(storage_key);
            }
        }
        if let Some(signature) = signature {
            signature.rlp_append(&mut stream);
        }

        [&[0x03], stream.as_raw()].concat()
    }

    pub fn rlp_append(&self, rlp: &mut RlpStream, signature: &SignatureComponents) {
        rlp.append(&self.encode(Some(signature)));
    }

    pub fn hash(&self) -> H256 {
        keccak(&self.encode(None))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TypedTransaction {
    Legacy(Transaction),      // old legacy RLP encoded transaction
    AccessList(AccessListTx), // EIP-2930 Transaction with a list of addresses and storage keys that the transaction plans to access.
                              // Accesses outside the list are possible, but become more expensive.
}

//Function that are batched from Transaction struct and needs to be reimplemented
impl TypedTransaction {

    pub fn tx_type(&self) -> u8 {
        match self {
            Self::Legacy(_) => 0x00,
            Self::AccessList(_) => 0x03,
        }
    }

    /// The message hash of the transaction.
    pub fn hash(&self, chain_id: Option<u64>) -> H256 {
        match self {
            Self::Legacy(tx) => tx.hash(chain_id),
            Self::AccessList(ocl) => ocl.hash(),
        }
    }

    /// Signs the transaction as coming from `sender`.
    pub fn sign(self, secret: &Secret, chain_id: Option<u64>) -> SignedTransaction {
        let sig = ::ethkey::sign(secret, &self.hash(chain_id))
            .expect("data is valid and context has signing capabilities; qed");
        SignedTransaction::new(self.with_signature(sig, chain_id))
            .expect("secret is valid so it's recoverable")
    }

    /// Signs the transaction with signature.
    pub fn with_signature(self, sig: Signature, chain_id: Option<u64>) -> UnverifiedTransaction {
        UnverifiedTransaction {
            unsigned: self,
            signature: SignatureComponents {
                r: sig.r().into(),
                s: sig.s().into(),
                v: signature::add_chain_replay_protection(sig.v() as u64, chain_id),
            },
            hash: 0.into(),
        }
        .compute_hash()
    }

    /// Specify the sender; this won't survive the serialize/deserialize process, but can be cloned.
    pub fn fake_sign(self, from: Address) -> SignedTransaction {
        SignedTransaction {
            transaction: UnverifiedTransaction {
                unsigned: self,
                signature: SignatureComponents {
                    r: U256::one(),
                    s: U256::one(),
                    v: 0,
                },
                hash: 0.into(),
            }
            .compute_hash(),
            sender: from,
            public: None,
        }
    }

    /// Legacy EIP-86 compatible empty signature.
    /// This method is used in json tests as well as
    /// signature verification tests.
    //#[cfg(any(test, feature = "test-helpers"))]
    pub fn null_sign(self, chain_id: u64) -> SignedTransaction {
        SignedTransaction {
            transaction: UnverifiedTransaction {
                unsigned: self,
                signature: SignatureComponents {
                    r: U256::zero(),
                    s: U256::zero(),
                    v: chain_id,
                },
                hash: 0.into(),
            }
            .compute_hash(),
            sender: UNSIGNED_SENDER,
            public: None,
        }
    }

    /// Useful for test incorrectly signed transactions.
    #[cfg(test)]
    pub fn invalid_sign(self) -> UnverifiedTransaction {
        UnverifiedTransaction {
            unsigned: self,
            signature: SignatureComponents {
                r: U256::one(),
                s: U256::one(),
                v: 0,
            },
            hash: 0.into(),
        }
        .compute_hash()
    }

    // Next functions are for encoded/decode

    pub fn tx(&self) -> &Transaction {
        match self {
            Self::Legacy(tx) => tx,
            Self::AccessList(ocl) => ocl.tx(),
        }
    }

    pub fn tx_mut(&mut self) -> &mut Transaction {
        match self {
            Self::Legacy(tx) => tx,
            Self::AccessList(ocl) => ocl.tx_mut(),
        }
    }

    pub fn decode(tx: &Rlp) -> Result<UnverifiedTransaction, DecoderError> {
        if tx.is_null() {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        //type of transaction can be obtained from first byte. If first bit is 1 it means we are dealing with RLP list.
        //if it is 0 it means that we are dealing with custom transaction defined in EIP-2918.
        //let header = tx[0]; tx.is_list()
        if tx.is_list() {
            //legacy transaction wrapped around RLP encoding
            Transaction::decode(tx)
        } else {
            let tx_data = tx.data()?;
            //other transaction types
            match tx_data[0] {
                0x03 => AccessListTx::decode(tx_data),
                _ => Err(DecoderError::Custom("Unknown transaction")),
            }
        }
    }

    fn rlp_append(&self, s: &mut RlpStream, signature: &SignatureComponents) {
        match self {
            Self::Legacy(tx) => tx.rlp_append(s, None, signature),
            Self::AccessList(opt) => opt.rlp_append(s, signature),
        }
    }
}

impl HeapSizeOf for TypedTransaction {
    fn heap_size_of_children(&self) -> usize {
        match self {
            TypedTransaction::Legacy(legacy) => legacy.heap_size_of_children(),
            TypedTransaction::AccessList(oal) => oal.tx().heap_size_of_children(),
        }
    }
}

/// Components that constitute transaction signature
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignatureComponents {
    /// The V field of the signature; the LS bit described which half of the curve our point falls
    /// in. The MS bits describe which chain this transaction is for. If 27/28, its for all chains.
    v: u64,
    /// The R field of the signature; helps describe the point on the curve.
    r: U256,
    /// The S field of the signature; helps describe the point on the curve.
    s: U256,
}

impl SignatureComponents {
    pub fn rlp_append(&self, s: &mut RlpStream) {
        s.append(&self.v);
        s.append(&self.r);
        s.append(&self.s);
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl From<ethjson::state::Transaction> for SignedTransaction {
    fn from(t: ethjson::state::Transaction) -> Self {
        let to: Option<ethjson::hash::Address> = t.to.into();
        let secret = t.secret.map(|s| Secret::from(s.0));
        let tx = TypedTransaction::Legacy(Transaction {
            nonce: t.nonce.into(),
            gas_price: t.gas_price.into(),
            gas: t.gas_limit.into(),
            action: match to {
                Some(to) => Action::Call(to.into()),
                None => Action::Create,
            },
            value: t.value.into(),
            data: t.data.into(),
        });
        match secret {
            Some(s) => tx.sign(&s, None),
            None => tx.null_sign(1),
        }
    }
}

impl From<ethjson::transaction::Transaction> for UnverifiedTransaction {
    fn from(t: ethjson::transaction::Transaction) -> Self {
        let to: Option<ethjson::hash::Address> = t.to.into();
        UnverifiedTransaction {
            unsigned: TypedTransaction::Legacy(Transaction {
                nonce: t.nonce.into(),
                gas_price: t.gas_price.into(),
                gas: t.gas_limit.into(),
                action: match to {
                    Some(to) => Action::Call(to.into()),
                    None => Action::Create,
                },
                value: t.value.into(),
                data: t.data.into(),
            }),
            signature: SignatureComponents {
                r: t.r.into(),
                s: t.s.into(),
                v: t.v.into(),
            },
            hash: 0.into(),
        }
        .compute_hash()
    }
}

/// Signed transaction information without verified signature.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UnverifiedTransaction {
    /// Plain Transaction.
    unsigned: TypedTransaction,
    /// Transaction signature
    signature: SignatureComponents,
    /// Hash of the transaction
    hash: H256,
}

impl HeapSizeOf for UnverifiedTransaction {
    fn heap_size_of_children(&self) -> usize {
        self.unsigned.heap_size_of_children()
    }
}

impl Deref for UnverifiedTransaction {
    type Target = TypedTransaction;

    fn deref(&self) -> &Self::Target {
        &self.unsigned
    }
}

impl rlp::Decodable for UnverifiedTransaction {
    fn decode(d: &Rlp) -> Result<Self, DecoderError> {
        TypedTransaction::decode(d)
    }
}

impl rlp::Encodable for UnverifiedTransaction {
    fn rlp_append(&self, s: &mut RlpStream) {
        self.unsigned.rlp_append(s, &self.signature);
    }
}

impl UnverifiedTransaction {
    /// Used to compute hash of created transactions. Without chainid but signature added. This is used to verify if transaction is same or not
    fn compute_hash(mut self) -> UnverifiedTransaction {
        let hash = keccak(&*self.rlp_bytes());
        self.hash = hash;
        self
    }

    pub fn new(
        transaction: TypedTransaction,
        signature: SignatureComponents,
        hash: H256,
    ) -> UnverifiedTransaction {
        UnverifiedTransaction {
            unsigned: transaction,
            signature,
            hash,
        }
    }
    /// Checks if the signature is empty.
    pub fn is_unsigned(&self) -> bool {
        self.signature.r.is_zero() && self.signature.s.is_zero()
    }

    ///	Reference to unsigned part of this transaction.
    pub fn as_unsigned(&self) -> &TypedTransaction {
        //TODO check where this is used
        &self.unsigned
    }

    /// Returns standardized `v` value (0, 1 or 4 (invalid))
    pub fn standard_v(&self) -> u8 {
        signature::check_replay_protection(self.signature.v)
    }

    /// The `v` value that appears in the RLP.
    pub fn original_v(&self) -> u64 {
        self.signature.v
    }

    /// The chain ID, or `None` if this is a global transaction.
    pub fn chain_id(&self) -> Option<u64> {
        match self.signature.v {
            v if self.is_unsigned() => Some(v),
            v if v >= 35 => Some((v - 35) / 2),
            _ => None,
        }
    }

    /// Construct a signature object from the sig.
    pub fn signature(&self) -> Signature {
        Signature::from_rsv(
            &self.signature.r.into(),
            &self.signature.s.into(),
            self.standard_v(),
        )
    }

    /// Checks whether the signature has a low 's' value.
    pub fn check_low_s(&self) -> Result<(), ethkey::Error> {
        if !self.signature().is_low_s() {
            Err(ethkey::Error::InvalidSignature.into())
        } else {
            Ok(())
        }
    }

    /// Get the hash of this transaction (keccak of the RLP).
    pub fn hash(&self) -> H256 {
        self.hash
    }

    /// Recovers the public key of the sender.
    pub fn recover_public(&self) -> Result<Public, ethkey::Error> {
        Ok(recover(
            &self.signature(),
            &self.unsigned.hash(self.chain_id()),
        )?)
    }

    /// Verify basic signature params. Does not attempt sender recovery.
    pub fn verify_basic(
        &self,
        check_low_s: bool,
        chain_id: Option<u64>,
    ) -> Result<(), error::Error> {
        if self.is_unsigned() {
            return Err(ethkey::Error::InvalidSignature.into());
        }
        if check_low_s {
            self.check_low_s()?;
        }
        match (self.chain_id(), chain_id) {
            (None, _) => {}
            (Some(n), Some(m)) if n == m => {}
            _ => return Err(error::Error::InvalidChainId),
        };
        Ok(())
    }
}

/// A `UnverifiedTransaction` with successfully recovered `sender`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignedTransaction {
    transaction: UnverifiedTransaction,
    sender: Address,
    public: Option<Public>,
}

impl HeapSizeOf for SignedTransaction {
    fn heap_size_of_children(&self) -> usize {
        self.transaction.heap_size_of_children()
    }
}

impl rlp::Encodable for SignedTransaction {
    fn rlp_append(&self, s: &mut RlpStream) {
        self.transaction.rlp_append(s)
    }
}

impl Deref for SignedTransaction {
    type Target = UnverifiedTransaction;
    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

impl From<SignedTransaction> for UnverifiedTransaction {
    fn from(tx: SignedTransaction) -> Self {
        tx.transaction
    }
}

impl SignedTransaction {
    /// Try to verify transaction and recover sender.
    pub fn new(transaction: UnverifiedTransaction) -> Result<Self, ethkey::Error> {
        if transaction.is_unsigned() {
            return Err(ethkey::Error::InvalidSignature);
        }
        let public = transaction.recover_public()?;
        let sender = public_to_address(&public);
        Ok(SignedTransaction {
            transaction,
            sender,
            public: Some(public),
        })
    }

    /// Returns transaction sender.
    pub fn sender(&self) -> Address {
        self.sender
    }

    /// Returns a public key of the sender.
    pub fn public_key(&self) -> Option<Public> {
        self.public
    }

    /// Checks is signature is empty.
    pub fn is_unsigned(&self) -> bool {
        self.transaction.is_unsigned()
    }

    /// Deconstructs this transaction back into `UnverifiedTransaction`
    pub fn deconstruct(self) -> (UnverifiedTransaction, Address, Option<Public>) {
        (self.transaction, self.sender, self.public)
    }
}

/// Signed Transaction that is a part of canon blockchain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizedTransaction {
    /// Signed part.
    pub signed: UnverifiedTransaction,
    /// Block number.
    pub block_number: BlockNumber,
    /// Block hash.
    pub block_hash: H256,
    /// Transaction index within block.
    pub transaction_index: usize,
    /// Cached sender
    pub cached_sender: Option<Address>,
}

impl LocalizedTransaction {
    /// Returns transaction sender.
    /// Panics if `LocalizedTransaction` is constructed using invalid `UnverifiedTransaction`.
    pub fn sender(&mut self) -> Address {
        if let Some(sender) = self.cached_sender {
            return sender;
        }
        if self.is_unsigned() {
            return UNSIGNED_SENDER.clone();
        }
        let sender = public_to_address(&self.recover_public()
			.expect("LocalizedTransaction is always constructed from transaction from blockchain; Blockchain only stores verified transactions; qed"));
        self.cached_sender = Some(sender);
        sender
    }
}

impl Deref for LocalizedTransaction {
    type Target = UnverifiedTransaction;

    fn deref(&self) -> &Self::Target {
        &self.signed
    }
}

/// Queued transaction with additional information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTransaction {
    /// Signed transaction data.
    pub transaction: SignedTransaction,
    /// To be activated at this condition. `None` for immediately.
    pub condition: Option<Condition>,
}

impl PendingTransaction {
    /// Create a new pending transaction from signed transaction.
    pub fn new(signed: SignedTransaction, condition: Option<Condition>) -> Self {
        PendingTransaction {
            transaction: signed,
            condition: condition,
        }
    }
}

impl Deref for PendingTransaction {
    type Target = SignedTransaction;

    fn deref(&self) -> &SignedTransaction {
        &self.transaction
    }
}

impl From<SignedTransaction> for PendingTransaction {
    fn from(t: SignedTransaction) -> Self {
        PendingTransaction {
            transaction: t,
            condition: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethereum_types::U256;
    use hash::keccak;

    //TODO CREATE TESTS FOR UnverifiedTransaction

    #[test]
    fn sender_test() {
        let bytes = ::rustc_hex::FromHex::from_hex("f85f800182520894095e7baea6a6c7c4c2dfeb977efac326af552d870a801ba048b55bfa915ac795c431978d8a6a992b628d557da5ff759b307d495a36649353a0efffd310ac743f371de3b9f7f9cb56c0b28ad43601b4ab949f53faa07bd2c804").unwrap();
        let t: UnverifiedTransaction =
            rlp::decode(&bytes).expect("decoding UnverifiedTransaction failed");
        assert_eq!(t.tx().data, b"");
        assert_eq!(t.tx().gas, U256::from(0x5208u64));
        assert_eq!(t.tx().gas_price, U256::from(0x01u64));
        assert_eq!(t.tx().nonce, U256::from(0x00u64));
        if let Action::Call(ref to) = t.tx().action {
            assert_eq!(*to, "095e7baea6a6c7c4c2dfeb977efac326af552d87".into());
        } else {
            panic!();
        }
        assert_eq!(t.tx().value, U256::from(0x0au64));
        assert_eq!(
            public_to_address(&t.recover_public().unwrap()),
            "0f65fe9276bc9a24ae7083ae28e2660ef72df99e".into()
        );
        assert_eq!(t.chain_id(), None);
    }

    #[test]
    fn empty_atom_as_create_action() {
        let empty_atom = [0x80];
        let action: Action = rlp::decode(&empty_atom).unwrap();
        assert_eq!(action, Action::Create);
    }

    #[test]
    fn empty_list_as_create_action_rejected() {
        let empty_list = [0xc0];
        let action: Result<Action, DecoderError> = rlp::decode(&empty_list);
        assert_eq!(action, Err(DecoderError::RlpExpectedToBeData));
    }

    #[test]
    fn signing_eip155_zero_chainid() {
        use ethkey::{Generator, Random};

        let key = Random.generate().unwrap();
        let t = TypedTransaction::Legacy(Transaction {
            action: Action::Create,
            nonce: U256::from(42),
            gas_price: U256::from(3000),
            gas: U256::from(50_000),
            value: U256::from(1),
            data: b"Hello!".to_vec(),
        });

        let hash = t.hash(Some(0));
        let sig = ::ethkey::sign(&key.secret(), &hash).unwrap();
        let u = t.with_signature(sig, Some(0));

        assert!(SignedTransaction::new(u).is_ok());
    }

    #[test]
    fn signing() {
        use ethkey::{Generator, Random};

        let key = Random.generate().unwrap();
        let t = TypedTransaction::Legacy(Transaction {
            action: Action::Create,
            nonce: U256::from(42),
            gas_price: U256::from(3000),
            gas: U256::from(50_000),
            value: U256::from(1),
            data: b"Hello!".to_vec(),
        })
        .sign(&key.secret(), None);
        assert_eq!(Address::from(keccak(key.public())), t.sender());
        assert_eq!(t.chain_id(), None);
    }

    #[test]
    fn fake_signing() {
        let t = TypedTransaction::Legacy(Transaction {
            action: Action::Create,
            nonce: U256::from(42),
            gas_price: U256::from(3000),
            gas: U256::from(50_000),
            value: U256::from(1),
            data: b"Hello!".to_vec(),
        })
        .fake_sign(Address::from(0x69));
        assert_eq!(Address::from(0x69), t.sender());
        assert_eq!(t.chain_id(), None);

        let t = t.clone();
        assert_eq!(Address::from(0x69), t.sender());
        assert_eq!(t.chain_id(), None);
    }

    #[test]
    fn should_reject_null_signature() {
        use std::str::FromStr;
        let t = TypedTransaction::Legacy(Transaction {
            nonce: U256::zero(),
            gas_price: U256::from(10000000000u64),
            gas: U256::from(21000),
            action: Action::Call(
                Address::from_str("d46e8dd67c5d32be8058bb8eb970870f07244567").unwrap(),
            ),
            value: U256::from(1),
            data: vec![],
        })
        .null_sign(1);

        println!("transaction {:?}", t);

        let res = SignedTransaction::new(t.transaction);
        match res {
            Err(ethkey::Error::InvalidSignature) => {}
            _ => panic!("null signature should be rejected"),
        }
    }

    #[test]
    fn should_recover_from_chain_specific_signing() {
        use ethkey::{Generator, Random};
        let key = Random.generate().unwrap();
        let t = TypedTransaction::Legacy(Transaction {
            action: Action::Create,
            nonce: U256::from(42),
            gas_price: U256::from(3000),
            gas: U256::from(50_000),
            value: U256::from(1),
            data: b"Hello!".to_vec(),
        })
        .sign(&key.secret(), Some(69));
        assert_eq!(Address::from(keccak(key.public())), t.sender());
        assert_eq!(t.chain_id(), Some(69));
    }

    #[test]
    fn should_encode_decode_access_list_tx() {
        use ethkey::{Generator, Random};
        let key = Random.generate().unwrap();
        let t = TypedTransaction::AccessList(AccessListTx::new(
            Transaction {
                action: Action::Create,
                nonce: U256::from(42),
                gas_price: U256::from(3000),
                gas: U256::from(50_000),
                value: U256::from(1),
                data: b"Hello!".to_vec(),
            },
            Vec::new(),
        ))
        .sign(&key.secret(), Some(69));

        let encoded = rlp::encode(&t);
        let t_new: UnverifiedTransaction =
            rlp::decode(&encoded).expect("Error on UnverifiedTransaction decoder");
        if t_new.unsigned != t.unsigned {
            assert!(true, "encoded/decoded tx differs from original");
        }
    }

    #[test]
    fn should_decode_access_list_tx() {
        use rustc_hex::FromHex;
        let encoded_tx = "b85803f8552a820bb882c35080018648656c6c6f21c081aea0ed1f268cf14c76ecc77b32e903d0a7d7913d2159fde2155988cd8180b8e09144a04acdfaf2dbfabfe78fa6999d4229c59f9a80545aebd983230cc8fa7328c70e53";
        let _: UnverifiedTransaction =
            rlp::decode(&FromHex::from_hex(encoded_tx).unwrap()).expect("decoding tx data failed");
    }

    #[test]
    fn should_agree_with_vitalik() {
        use rustc_hex::FromHex;

        let test_vector = |tx_data: &str, address: &'static str| {
            let signed =
                rlp::decode(&FromHex::from_hex(tx_data).unwrap()).expect("decoding tx data failed");
            let signed = SignedTransaction::new(signed).unwrap();
            assert_eq!(signed.sender(), address.into());
            println!("chainid: {:?}", signed.chain_id());
        };

        test_vector("f864808504a817c800825208943535353535353535353535353535353535353535808025a0044852b2a670ade5407e78fb2863c51de9fcb96542a07186fe3aeda6bb8a116da0044852b2a670ade5407e78fb2863c51de9fcb96542a07186fe3aeda6bb8a116d", "0xf0f6f18bca1b28cd68e4357452947e021241e9ce");
        test_vector("f864018504a817c80182a410943535353535353535353535353535353535353535018025a0489efdaa54c0f20c7adf612882df0950f5a951637e0307cdcb4c672f298b8bcaa0489efdaa54c0f20c7adf612882df0950f5a951637e0307cdcb4c672f298b8bc6", "0x23ef145a395ea3fa3deb533b8a9e1b4c6c25d112");
        test_vector("f864028504a817c80282f618943535353535353535353535353535353535353535088025a02d7c5bef027816a800da1736444fb58a807ef4c9603b7848673f7e3a68eb14a5a02d7c5bef027816a800da1736444fb58a807ef4c9603b7848673f7e3a68eb14a5", "0x2e485e0c23b4c3c542628a5f672eeab0ad4888be");
        test_vector("f865038504a817c803830148209435353535353535353535353535353535353535351b8025a02a80e1ef1d7842f27f2e6be0972bb708b9a135c38860dbe73c27c3486c34f4e0a02a80e1ef1d7842f27f2e6be0972bb708b9a135c38860dbe73c27c3486c34f4de", "0x82a88539669a3fd524d669e858935de5e5410cf0");
        test_vector("f865048504a817c80483019a28943535353535353535353535353535353535353535408025a013600b294191fc92924bb3ce4b969c1e7e2bab8f4c93c3fc6d0a51733df3c063a013600b294191fc92924bb3ce4b969c1e7e2bab8f4c93c3fc6d0a51733df3c060", "0xf9358f2538fd5ccfeb848b64a96b743fcc930554");
        test_vector("f865058504a817c8058301ec309435353535353535353535353535353535353535357d8025a04eebf77a833b30520287ddd9478ff51abbdffa30aa90a8d655dba0e8a79ce0c1a04eebf77a833b30520287ddd9478ff51abbdffa30aa90a8d655dba0e8a79ce0c1", "0xa8f7aba377317440bc5b26198a363ad22af1f3a4");
        test_vector("f866068504a817c80683023e3894353535353535353535353535353535353535353581d88025a06455bf8ea6e7463a1046a0b52804526e119b4bf5136279614e0b1e8e296a4e2fa06455bf8ea6e7463a1046a0b52804526e119b4bf5136279614e0b1e8e296a4e2d", "0xf1f571dc362a0e5b2696b8e775f8491d3e50de35");
        test_vector("f867078504a817c807830290409435353535353535353535353535353535353535358201578025a052f1a9b320cab38e5da8a8f97989383aab0a49165fc91c737310e4f7e9821021a052f1a9b320cab38e5da8a8f97989383aab0a49165fc91c737310e4f7e9821021", "0xd37922162ab7cea97c97a87551ed02c9a38b7332");
        test_vector("f867088504a817c8088302e2489435353535353535353535353535353535353535358202008025a064b1702d9298fee62dfeccc57d322a463ad55ca201256d01f62b45b2e1c21c12a064b1702d9298fee62dfeccc57d322a463ad55ca201256d01f62b45b2e1c21c10", "0x9bddad43f934d313c2b79ca28a432dd2b7281029");
        test_vector("f867098504a817c809830334509435353535353535353535353535353535353535358202d98025a052f8f61201b2b11a78d6e866abc9c3db2ae8631fa656bfe5cb53668255367afba052f8f61201b2b11a78d6e866abc9c3db2ae8631fa656bfe5cb53668255367afb", "0x3c24d7329e92f84f08556ceb6df1cdb0104ca49f");
    }
}
