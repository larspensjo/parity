// Copyright 2015, 2016 Ethcore (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use std::collections::{VecDeque, HashSet};
use lru_cache::LruCache;
use util::journaldb::JournalDB;
use util::hash::{H256};
use util::hashdb::HashDB;
use account::Account;
use header::BlockNumber;
use util::{Arc, Address, Database, DBTransaction, UtilError, Mutex, Hashable, BytesConvertable};
use bloomfilter::{Bloom, BloomJournal};
use client::DB_COL_ACCOUNT_BLOOM;
use byteorder::{LittleEndian, ByteOrder};

const STATE_CACHE_ITEMS: usize = 65536;
const STATE_CACHE_BLOCKS: usize = 8;


pub const ACCOUNT_BLOOM_SPACE: usize = 1048576;
pub const DEFAULT_ACCOUNT_PRESET: usize = 1000000;

pub const ACCOUNT_BLOOM_HASHCOUNT_KEY: &'static [u8] = b"account_hash_count";

/// Shared canonical state cache.
struct AccountCache {
	/// DB Account cache. `None` indicates that account is known to be missing.
	accounts: LruCache<Address, Option<Account>>,
	/// Accounts changed in recently committed blocks. Ordered by block number.
	modifications: VecDeque<BlockChanges>,
}

/// Pending account cache item.
struct CacheQueueItem {
	/// Account address.
	address: Address,
	/// Acccount data or `None` if account does not exist.
	account: Option<Account>,
	/// Indicates that the account was modified before being
	/// added to the cache.
	modified: bool,
}

#[derive(Debug)]
/// Accumulates a list of accounts changed in a block.
struct BlockChanges {
	/// Block number.
	number: BlockNumber,
	/// Block hash.
	hash: H256,
	/// Parent block hash.
	parent: H256,
	/// A set of modified account addresses.
	accounts: HashSet<Address>,
	/// Block is part of the canonical chain.
	is_canon: bool,
}

/// State database abstraction.
/// Manages shared global state cache.
/// A clone of `StateDB` may be created as canonical or not.
/// For canonical clones cache changes are accumulated and applied
/// on commit.
/// For non-canonical clones cache is cleared on commit.
pub struct StateDB {
	/// Backing database.
	db: Box<JournalDB>,
	/// Shared canonical state cache.
	account_cache: Arc<Mutex<AccountCache>>,
	/// Local pending cache changes.
	pending_cache: Vec<CacheQueueItem>,
	/// Shared account bloom. Does not handle chain reorganizations.
	account_bloom: Arc<Mutex<Bloom>>,
	/// Hash of the block on top of which this instance was created or
	/// `None` if cache is disabled
	parent_hash: Option<H256>,
	/// Hash of the committing block or `None` if not committed yet.
	commit_hash: Option<H256>,
	/// Number of the committing block or `None` if not committed yet.
	commit_number: Option<BlockNumber>,
}

pub const ACCOUNT_BLOOM_SPACE: usize = 1048576;
pub const DEFAULT_ACCOUNT_PRESET: usize = 1000000;

pub const ACCOUNT_BLOOM_HASHCOUNT_KEY: &'static [u8] = b"account_hash_count";

impl StateDB {
	/// Loads accounts bloom from the database
	/// This bloom is used to handle request for the non-existant account fast
	pub fn load_bloom(db: &Database) -> Bloom {
		let hash_count_entry = db.get(DB_COL_ACCOUNT_BLOOM, ACCOUNT_BLOOM_HASHCOUNT_KEY)
			.expect("Low-level database error");

		if hash_count_entry.is_none() {
			return Bloom::new(ACCOUNT_BLOOM_SPACE, DEFAULT_ACCOUNT_PRESET);
		}
		let hash_count_bytes = hash_count_entry.unwrap();
		assert_eq!(hash_count_bytes.len(), 1);
		let hash_count = hash_count_bytes[0];

		let mut bloom_parts = vec![0u64; ACCOUNT_BLOOM_SPACE / 8];
		let mut key = [0u8; 8];
		for i in 0..ACCOUNT_BLOOM_SPACE / 8 {
			LittleEndian::write_u64(&mut key, i as u64);
			bloom_parts[i] = db.get(DB_COL_ACCOUNT_BLOOM, &key).expect("low-level database error")
				.and_then(|val| Some(LittleEndian::read_u64(&val[..])))
				.unwrap_or(0u64);
		}

		let bloom = Bloom::from_parts(&bloom_parts, hash_count as u32);
		trace!(target: "account_bloom", "Bloom is {:?} full, hash functions count = {:?}", bloom.how_full(), hash_count);
		bloom
	}

	/// Create a new instance wrapping `JournalDB`
	pub fn new(db: Box<JournalDB>) -> StateDB {
		let bloom = Self::load_bloom(db.backing());
		StateDB {
			db: db,
			account_cache: Arc::new(Mutex::new(AccountCache {
				accounts: LruCache::new(STATE_CACHE_ITEMS),
				modifications: VecDeque::new(),
			})),
			pending_cache: Vec::new(),
			account_bloom: Arc::new(Mutex::new(bloom)),
			parent_hash: None,
			commit_hash: None,
			commit_number: None,
		}
	}

	pub fn check_account_bloom(&self, address: &Address) -> bool {
		trace!(target: "account_bloom", "Check account bloom: {:?}", address);
		let bloom = self.account_bloom.lock();
		bloom.check(address.sha3().as_slice())
	}

	pub fn note_account_bloom(&self, address: &Address) {
		trace!(target: "account_bloom", "Note account bloom: {:?}", address);
		let mut bloom = self.account_bloom.lock();
		bloom.set(address.sha3().as_slice());
	}

	pub fn commit_bloom(batch: &DBTransaction, journal: BloomJournal) -> Result<(), UtilError> {
		assert!(journal.hash_functions <= 255);
		try!(batch.put(DB_COL_ACCOUNT_BLOOM, ACCOUNT_BLOOM_HASHCOUNT_KEY, &vec![journal.hash_functions as u8]));
		let mut key = [0u8; 8];
		let mut val = [0u8; 8];

		for (bloom_part_index, bloom_part_value) in journal.entries {
			LittleEndian::write_u64(&mut key, bloom_part_index as u64);
			LittleEndian::write_u64(&mut val, bloom_part_value);
			try!(batch.put(DB_COL_ACCOUNT_BLOOM, &key, &val));
		}
		Ok(())
	}

	/// Commit all recent insert operations and canonical historical commits' removals from the
	/// old era to the backing database, reverting any non-canonical historical commit's inserts.
	pub fn commit(&mut self, batch: &DBTransaction, now: u64, id: &H256, end: Option<(u64, H256)>) -> Result<u32, UtilError> {
		{
			let mut bloom_lock = self.account_bloom.lock();
			try!(Self::commit_bloom(batch, bloom_lock.drain_journal()));
		}

		let records = try!(self.db.commit(batch, now, id, end));
		self.commit_hash = Some(id.clone());
		self.commit_number = Some(now);
		Ok(records)
	}

	/// Apply pending cache changes and synchronize canonical
	/// state cache with the best block state.
	/// This function updates the cache by removing entries that are
	/// invalidated by chain reorganization. `update_cache` should be
	/// called after the block has been commited and the blockchain
	/// route has ben calculated.
	pub fn sync_cache(&mut self, enacted: &[H256], retracted: &[H256], is_best: bool) {
		trace!("sync_cache id = (#{:?}, {:?}), parent={:?}, best={}", self.commit_number, self.commit_hash, self.parent_hash, is_best);
		let mut cache = self.account_cache.lock();
		let mut cache = &mut *cache;

		// Clean changes from re-enacted and retracted blocks
		let mut clear = false;
		for block in enacted.iter().filter(|h| self.commit_hash.as_ref().map_or(false, |p| *h != p)) {
			clear = clear || {
				if let Some(ref mut m) = cache.modifications.iter_mut().find(|ref m| &m.hash == block) {
					trace!("Reverting enacted block {:?}", block);
					m.is_canon = true;
					for a in &m.accounts {
						trace!("Reverting enacted address {:?}", a);
						cache.accounts.remove(a);
					}
					false
				} else {
					true
				}
			};
		}

		for block in retracted {
			clear = clear || {
				if let Some(ref mut m) = cache.modifications.iter_mut().find(|ref m| &m.hash == block) {
					trace!("Retracting block {:?}", block);
					m.is_canon = false;
					for a in &m.accounts {
						trace!("Retracted address {:?}", a);
						cache.accounts.remove(a);
					}
					false
				} else {
					true
				}
			};
		}
		if clear {
			// We don't know anything about the block; clear everything
			trace!("Wiping cache");
			cache.accounts.clear();
			cache.modifications.clear();
		}

		// Apply cache changes only if committing on top of the latest canonical state
		// blocks are ordered by number and only one block with a given number is marked as canonical
		// (contributed to canonical state cache)
		if let (Some(ref number), Some(ref hash), Some(ref parent)) = (self.commit_number, self.commit_hash, self.parent_hash) {
			if cache.modifications.len() == STATE_CACHE_BLOCKS {
				cache.modifications.pop_back();
			}
			let mut modifications = HashSet::new();
			trace!("committing {} cache entries", self.pending_cache.len());
			for account in self.pending_cache.drain(..) {
				if account.modified {
					modifications.insert(account.address.clone());
				}
				if is_best {
					if let Some(&mut Some(ref mut existing)) = cache.accounts.get_mut(&account.address) {
						if let Some(new) = account.account {
							if account.modified {
								existing.overwrite_with(new);
							}
							continue;
						}
					}
					cache.accounts.insert(account.address, account.account);
				}
			}

			// Save modified accounts. These are ordered by the block number.
			let block_changes = BlockChanges {
				accounts: modifications,
				number: *number,
				hash: hash.clone(),
				is_canon: is_best,
				parent: parent.clone(),
			};
			let insert_at = cache.modifications.iter().enumerate().find(|&(_, ref m)| m.number < *number).map(|(i, _)| i);
			trace!("inserting modifications at {:?}", insert_at);
			if let Some(insert_at) = insert_at {
				cache.modifications.insert(insert_at, block_changes);
			} else {
				cache.modifications.push_back(block_changes);
			}
		}
	}

	/// Returns an interface to HashDB.
	pub fn as_hashdb(&self) -> &HashDB {
		self.db.as_hashdb()
	}

	/// Returns an interface to mutable HashDB.
	pub fn as_hashdb_mut(&mut self) -> &mut HashDB {
		self.db.as_hashdb_mut()
	}

	/// Clone the database.
	pub fn boxed_clone(&self) -> StateDB {
		StateDB {
			db: self.db.boxed_clone(),
			account_cache: self.account_cache.clone(),
			pending_cache: Vec::new(),
			account_bloom: self.account_bloom.clone(),
			parent_hash: None,
			commit_hash: None,
			commit_number: None,
		}
	}

	/// Clone the database for a canonical state.
	pub fn boxed_clone_canon(&self, parent: &H256) -> StateDB {
		StateDB {
			db: self.db.boxed_clone(),
			account_cache: self.account_cache.clone(),
			pending_cache: Vec::new(),
			account_bloom: self.account_bloom.clone(),
			parent_hash: Some(parent.clone()),
			commit_hash: None,
			commit_number: None,
		}
	}

	/// Check if pruning is enabled on the database.
	pub fn is_pruned(&self) -> bool {
		self.db.is_pruned()
	}

	/// Heap size used.
	pub fn mem_used(&self) -> usize {
		self.db.mem_used() //TODO: + self.account_cache.lock().heap_size_of_children()
	}

	/// Returns underlying `JournalDB`.
	pub fn journal_db(&self) -> &JournalDB {
		&*self.db
	}

	/// Add pending cache change.
	/// The change is queued to be applied in `commit`.
	pub fn add_to_account_cache(&mut self, addr: Address, data: Option<Account>, modified: bool) {
		self.pending_cache.push(CacheQueueItem {
			address: addr,
			account: data,
			modified: modified,
		})
	}

	/// Get basic copy of the cached account. Does not include storage.
	/// Returns 'None' if cache is disabled or if the account is not cached.
	pub fn get_cached_account(&self, addr: &Address) -> Option<Option<Account>> {
		let mut cache = self.account_cache.lock();
		if !Self::is_allowed(addr, &self.parent_hash, &cache.modifications) {
			return None;
		}
		cache.accounts.get_mut(&addr).map(|a| a.as_ref().map(|a| a.clone_basic()))
	}

	/// Get value from a cached account.
	/// Returns 'None' if cache is disabled or if the account is not cached.
	pub fn get_cached<F, U>(&self, a: &Address, f: F) -> Option<U>
		where F: FnOnce(Option<&mut Account>) -> U {
		let mut cache = self.account_cache.lock();
		if !Self::is_allowed(a, &self.parent_hash, &cache.modifications) {
			return None;
		}
		cache.accounts.get_mut(a).map(|c| f(c.as_mut()))
	}

	/// Check if the account can be returned from cache by matching current block parent hash against canonical
	/// state and filtering out account modified in later blocks.
	fn is_allowed(addr: &Address, parent_hash: &Option<H256>, modifications: &VecDeque<BlockChanges>) -> bool {
		let mut parent = match *parent_hash {
			None => {
				trace!("Cache lookup skipped for {:?}: no parent hash", addr);
				return false;
			}
			Some(ref parent) => parent,
		};
		if modifications.is_empty() {
			return true;
		}
		// Ignore all accounts modified in later blocks
		// Modifications contains block ordered by the number
		// We search for our parent in that list first and then for
		// all its parent until we hit the canonical block,
		// checking against all the intermediate modifications.
		let mut iter = modifications.iter();
		while let Some(ref m) = iter.next() {
			if &m.hash == parent {
				if m.is_canon {
					return true;
				}
				parent = &m.parent;
			}
			if m.accounts.contains(addr) {
				trace!("Cache lookup skipped for {:?}: modified in a later block", addr);
				return false;
			}
		}
		trace!("Cache lookup skipped for {:?}: parent hash is unknown", addr);
		return false;
	}
}

#[cfg(test)]
mod tests {

use util::{U256, H256, FixedHash, Address, DBTransaction};
use tests::helpers::*;
use state::Account;
use util::log::init_log;

#[test]
fn state_db_smoke() {
	init_log();

	let mut state_db_result = get_temp_state_db();
	let state_db = state_db_result.take();
	let root_parent = H256::random();
	let address = Address::random();
	let h0 = H256::random();
	let h1a = H256::random();
	let h1b = H256::random();
	let h2a = H256::random();
	let h2b = H256::random();
	let h3a = H256::random();
	let h3b = H256::random();
	let mut batch = DBTransaction::new(state_db.journal_db().backing());

	// blocks  [ 3a(c) 2a(c) 2b 1b 1a(c) 0 ]
    // balance [ 5     5     4  3  2     2 ]
	let mut s = state_db.boxed_clone_canon(&root_parent);
	s.add_to_account_cache(address, Some(Account::new_basic(2.into(), 0.into())), false);
	s.commit(&mut batch, 0, &h0, None).unwrap();
	s.sync_cache(&[], &[], true);

	let mut s = state_db.boxed_clone_canon(&h0);
	s.commit(&mut batch, 1, &h1a, None).unwrap();
	s.sync_cache(&[], &[], true);

	let mut s = state_db.boxed_clone_canon(&h0);
	s.add_to_account_cache(address, Some(Account::new_basic(3.into(), 0.into())), true);
	s.commit(&mut batch, 1, &h1b, None).unwrap();
	s.sync_cache(&[], &[], false);

	let mut s = state_db.boxed_clone_canon(&h1b);
	s.add_to_account_cache(address, Some(Account::new_basic(4.into(), 0.into())), true);
	s.commit(&mut batch, 2, &h2b, None).unwrap();
	s.sync_cache(&[], &[], false);

	let mut s = state_db.boxed_clone_canon(&h1a);
	s.add_to_account_cache(address, Some(Account::new_basic(5.into(), 0.into())), true);
	s.commit(&mut batch, 2, &h2a, None).unwrap();
	s.sync_cache(&[], &[], true);

	let mut s = state_db.boxed_clone_canon(&h2a);
	s.commit(&mut batch, 3, &h3a, None).unwrap();
	s.sync_cache(&[], &[], true);

	let s = state_db.boxed_clone_canon(&h3a);
	assert_eq!(s.get_cached_account(&address).unwrap().unwrap().balance(), &U256::from(5));

	let s = state_db.boxed_clone_canon(&h1a);
	assert!(s.get_cached_account(&address).is_none());

	let s = state_db.boxed_clone_canon(&h2b);
	assert!(s.get_cached_account(&address).is_none());

	let s = state_db.boxed_clone_canon(&h1b);
	assert!(s.get_cached_account(&address).is_none());

	// reorg to 3b
	// blocks  [ 3b(c) 3a 2a 2b(c) 1b 1a 0 ]
	let mut s = state_db.boxed_clone_canon(&h2b);
	s.commit(&mut batch, 3, &h3b, None).unwrap();
	s.sync_cache(&[h1b.clone(), h2b.clone(), h3b.clone()], &[h1a.clone(), h2a.clone(), h3a.clone()], true);
	let s = state_db.boxed_clone_canon(&h3a);
	assert!(s.get_cached_account(&address).is_none());
}
}

