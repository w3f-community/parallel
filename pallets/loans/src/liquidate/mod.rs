// Copyright 2021 Parallel Finance Developer.
// This file is part of Parallel Finance.

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The liquidate implementation.

pub mod crypto;

use crate::pallet::*;
use primitives::{Balance, CurrencyId, PriceFeeder};
use frame_support::{
	log, pallet_prelude::*,
};
use frame_system::offchain::{ForAny, SendSignedTransaction, Signer};
use sp_runtime::{
    offchain::{
        storage_lock::{StorageLock, Time},
        Duration,
    },
	traits::{CheckedAdd, CheckedMul, Zero},
    FixedPointNumber, FixedU128,
};
use sp_std::collections::btree_map::BTreeMap;
use sp_std::prelude::*;

/// The miscellaneous information when transforming borrow records.
#[derive(Clone, Debug)]
struct BorrowMisc {
    currency: CurrencyId,
    amount: Balance,
    value: FixedU128,
}

/// The miscellaneous information when transforming collateral records.
#[derive(Clone, Debug)]
struct CollateralMisc {
    currency: CurrencyId,
    amount: Balance,
    value: FixedU128,
}

impl<T: Config> Pallet<T> {
    pub(crate) fn liquidate(_block_number: T::BlockNumber) -> Result<(), Error<T>> {
        let mut lock = StorageLock::<Time>::with_deadline(
            b"liquidate::lock",
            Duration::from_millis(T::LockPeriod::get()),
        );
        if let Err(_) = lock.try_lock() {
            return Err(Error::<T>::GetLockFailed);
        }

        let signer = Signer::<T, T::AuthorityId>::any_account();
        if !signer.can_sign() {
            return Err(Error::<T>::NoAvailableAccount);
        }

        let aggregated_account_borrows = Self::transform_account_borrows()?;

        let aggregated_account_collatoral = Self::transform_account_collateral()?;

        Self::liquidate_underwater_accounts(
            &signer,
            aggregated_account_borrows,
            aggregated_account_collatoral,
        )?;

        Ok(())
    }

    fn transform_account_borrows(
    ) -> Result<BTreeMap<T::AccountId, (FixedU128, Vec<BorrowMisc>)>, Error<T>> {
        let result = AccountBorrows::<T>::iter().fold(
            BTreeMap::<T::AccountId, (FixedU128, Vec<BorrowMisc>)>::new(),
            |mut acc, (k1, k2, snapshot)| {
                let loans_value = match T::PriceFeeder::get_price(&k1).and_then(|price_info| {
                    let result = Self::borrow_balance_stored_with_snapshot(
                        &k1, snapshot,
                    );
                    price_info
                        .0
                        .checked_mul(&FixedU128::from_inner(result.ok()?))
                }) {
                    None => {
                        acc.remove(&k2);
                        return acc;
                    }
                    Some(v) => v,
                };
                let default = (FixedU128::zero(), Vec::new());
                let existing = acc.get(&k2).unwrap_or(&default);
                let total_loans_value: FixedU128;
                if let Some(loans_value) = existing.0.checked_add(&loans_value) {
                    total_loans_value = loans_value;
                } else {
                    return acc;
                }
                let mut loans_detail = existing.1.clone();
                loans_detail.push(BorrowMisc {
                    currency: k1,
                    amount: snapshot.principal,
                    value: loans_value,
                });
                acc.insert(k2, (total_loans_value, loans_detail));
                acc
            },
        );

        Ok(result)
    }

    fn transform_account_collateral(
    ) -> Result<BTreeMap<T::AccountId, (FixedU128, Vec<CollateralMisc>)>, Error<T>> {
        let iter = AccountDeposits::<T>::iter();
        let result = iter.filter(|(.., deposits)| deposits.is_collateral).fold(
            BTreeMap::<T::AccountId, (FixedU128, Vec<CollateralMisc>)>::new(),
            |mut acc, (k1, k2, deposits)| {
                let balance = match ExchangeRate::<T>::get(&k1)
                    .checked_mul_int(deposits.voucher_balance)
                {
                    None => {
                        acc.remove(&k2);
                        return acc;
                    }
                    Some(v) => v,
                };
                let collateral_value = match T::PriceFeeder::get_price(&k1).and_then(|price_info| {
                    price_info.0.checked_mul(&FixedU128::from_inner(balance))
                }) {
                    None => {
                        acc.remove(&k2);
                        return acc;
                    }
                    Some(v) => v,
                };
                let under_collatoral_value = match collateral_value
                    .checked_mul(&CollateralFactor::<T>::get(&k1).into())
                {
                    None => {
                        acc.remove(&k2);
                        return acc;
                    }
                    Some(v) => v,
                };

                let default = (FixedU128::zero(), Vec::new());
                let existing = acc.get(&k2).unwrap_or(&default);
                let totoal_under_collatoral_value = existing.0 + under_collatoral_value;
                let mut collatoral_detail = existing.1.clone();
                collatoral_detail.push(CollateralMisc {
                    currency: k1,
                    amount: balance,
                    value: collateral_value,
                });
                acc.insert(k2, (totoal_under_collatoral_value, collatoral_detail));
                acc
            },
        );

        Ok(result)
    }

    fn liquidate_underwater_accounts(
        signer: &Signer<T, <T as Config>::AuthorityId, ForAny>,
        aggregated_account_borrows: BTreeMap<T::AccountId, (FixedU128, Vec<BorrowMisc>)>,
        aggregated_account_collatoral: BTreeMap<T::AccountId, (FixedU128, Vec<CollateralMisc>)>,
    ) -> Result<(), Error<T>> {
        aggregated_account_borrows.iter().for_each(
            |(account, (total_loans_value, loans_detail))| {
                let collateral = match aggregated_account_collatoral.get(account) {
                    None => return,
                    Some(v) => v,
                };

                // Borrower should not be liquidated if health factor is higher than 1
                if total_loans_value < &collateral.0 {
                    return;
                }

                let mut new_loans_detail = loans_detail.clone();
                new_loans_detail.sort_by(|a, b| a.value.cmp(&b.value));
                let liquidate_loans = &new_loans_detail[0];

                if let Some(item) = collateral.1.iter().find(|collateral_item| {
                    collateral_item.value.into_inner()
                        >= T::LiquidateFactor::get().mul_floor(liquidate_loans.value.into_inner())
                }) {
                    Self::submit_liquidate_transaction(
                        signer,
                        account.clone(),
                        liquidate_loans.currency,
                        T::LiquidateFactor::get().mul_floor(liquidate_loans.amount),
                        item.currency,
                    );
                }
            },
        );

        Ok(())
    }

    fn submit_liquidate_transaction(
        signer: &Signer<T, <T as Config>::AuthorityId, ForAny>,
        borrower: T::AccountId,
        loan_currency: CurrencyId,
        liquidation_value: Balance,
        collateral_currency: CurrencyId,
    ) {
        match signer.send_signed_transaction(|_account| {
            Call::liquidate_borrow(
                borrower.clone(),
                loan_currency.clone(),
                liquidation_value.clone(),
                collateral_currency.clone(),
            )
        }) {
            None => log::info!("No available accounts for liquidation"),
            Some((acc, Ok(()))) => log::info!(
                "[{:?}] Submitted liquidate borrow, borrower: {:?}",
                acc.id,
                borrower
            ),
            Some((acc, Err(e))) => {
                log::error!("[{:?}] Failed to submit transaction: {:?}", acc.id, e)
            }
        }
    }
}
