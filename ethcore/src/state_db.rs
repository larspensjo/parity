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

use lru_cache::LruCache;
use util::journaldb::JournalDB;
use util::hash::{H256};
use util::hashdb::HashDB;
use util::{Arc, Address, DBTransaction, UtilError, Mutex};
use account::Account;

struct AccountCache {
	accounts: LruCache<Address, Option<Account>>,
}

/// State database abstraction.
pub struct StateDB {
	db: Box<JournalDB>,
	account_cache: Arc<Mutex<AccountCache>>,
	cache_overlay: Vec<(Address, Option<Account>)>,
	is_canon: bool,
}

impl StateDB {
	/// Create a new instance wrapping `JournalDB`
	pub fn new(db: Box<JournalDB>) -> StateDB {
		StateDB {
			db: db,
			account_cache: Arc::new(Mutex::new(AccountCache { accounts: LruCache::new(65536) })),
			cache_overlay: Vec::new(),
			is_canon: false,
		}
	}

	/// Commit all recent insert operations and canonical historical commits' removals from the
	/// old era to the backing database, reverting any non-canonical historical commit's inserts.
	pub fn commit(&mut self, batch: &DBTransaction, now: u64, id: &H256, end: Option<(u64, H256)>) -> Result<u32, UtilError> {
		self.db.commit(batch, now, id, end)
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
			cache_overlay: Vec::new(),
			is_canon: false,
		}
	}

	/// Clone the database for a canonical state.
	pub fn canon_clone(&self) -> StateDB {
		StateDB {
			db: self.db.boxed_clone(),
			account_cache: self.account_cache.clone(),
			cache_overlay: Vec::new(),
			is_canon: true,
		}
	}

	/// Check if pruning is enabled on the database.
	pub fn is_pruned(&self) -> bool {
		self.db.is_pruned()
	}

	/// Heap size used.
	pub fn mem_used(&self) -> usize {
		self.db.mem_used()
	}

	/// Returns underlying `JournalDB`.
	pub fn journal_db(&self) -> &JournalDB {
		&*self.db
	}

	pub fn cache_account(&mut self, addr: Address, data: Option<Account>) {
		self.cache_overlay.push((addr, data));
	}

	pub fn commit_cache(&mut self) {
		let mut cache = self.account_cache.lock();
		for (address, account) in self.cache_overlay.drain(..) {
			if let Some(&mut Some(ref mut existing)) = cache.accounts.get_mut(&address) {
				if let Some(new) = account {
					existing.merge_with(new);
					continue;
				}
			}
			cache.accounts.insert(address, account);
		}
	}

	pub fn clear_cache(&self) {
		let mut cache = self.account_cache.lock();
		cache.accounts.clear();
	}

	pub fn get_cached_account(&self, addr: &Address) -> Option<Option<Account>> {
		if !self.is_canon {
			return None;
		}
		let mut cache = self.account_cache.lock();
		cache.accounts.get_mut(&addr).map(|a| a.as_ref().map(|a| a.clone_basic()))
	}

	pub fn get_cached<F, U>(&self, a: &Address, f: F) -> Option<U>
		where F: FnOnce(Option<&mut Account>) -> U {
		if !self.is_canon {
			return None;
		}
		let mut cache = self.account_cache.lock();
		cache.accounts.get_mut(a).map(|c| f(c.as_mut()))
	}
}
