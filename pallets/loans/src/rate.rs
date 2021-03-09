use frame_system::pallet_prelude::*;
use primitives::{Balance, CurrencyId};
use sp_runtime::{
    traits::{Zero},
    DispatchResult,
};
use sp_std::prelude::*;
use sp_std::{convert::TryInto, result, vec::{Vec}};

use crate::*;

const BLOCK_PER_YEAR: u128 = 5256000;
// const BLOCK_PER_YEAR: u128 = 2102400;
const DECIMAL: u128 = 1_000_000_000_000_000_000;

impl<T: Config> Pallet<T> {
    fn insert_borrow_rate(currency_id: CurrencyId, rate: u128) {
        BorrowRate::<T>::insert(currency_id, rate.clone());
        Self::deposit_event(Event::BorrowRateUpdated(currency_id, rate));
    }

    fn insert_supply_rate(currency_id: CurrencyId, rate: u128) {
        SupplyRate::<T>::insert(currency_id, rate.clone());
        Self::deposit_event(Event::SupplyRateUpdated(currency_id, rate));
    }

    pub fn to_decimal(n: Option<u128>) -> u128 {
        n.and_then(|r| r.checked_div(DECIMAL)).unwrap()
    }

    pub fn utilization_rate(cash: Balance, borrows: Balance, reserves: Balance) -> u128 {
        // Utilization rate is 0 when there are no borrows
        if borrows.is_zero() {
            return Zero::zero();
        }

        let total = cash
            .checked_add(borrows)
            .and_then(|r| r.checked_sub(reserves))
            .unwrap();

        borrows
            .checked_div(total)
            .unwrap()
    }

    pub fn update_jump_rate_model(
        base_rate_per_year: Balance,
        multiplier_per_year: Balance,
        jump_multiplier_per_year: Balance,
        kink: Balance,
    ) -> DispatchResult {
        let base = base_rate_per_year.checked_div(BLOCK_PER_YEAR).unwrap();

        let multiplier = multiplier_per_year
            .checked_mul(DECIMAL)
            .and_then(|r| r.checked_div(BLOCK_PER_YEAR.checked_mul(kink).unwrap()))
            .unwrap();

        let jump = jump_multiplier_per_year
            .checked_div(BLOCK_PER_YEAR)
            .unwrap();

        BaseRatePerBlock::<T>::put(Some(base));
        MultiplierPerBlock::<T>::put(Some(multiplier));
        JumpMultiplierPerBlock::<T>::put(Some(jump));
        Kink::<T>::put(Some(kink));

        Self::deposit_event(Event::NewInterestParams(base, multiplier, jump, kink));
        Ok(())
    }

    pub fn update_borrow_rate(
        currency_id: CurrencyId,
        cash: Balance,
        borrows: Balance,
        reserves: Balance,
    ) -> DispatchResult {
        let util = Self::utilization_rate(cash, borrows, reserves);
        UtilityRate::<T>::insert(currency_id, util);
        Self::deposit_event(Event::UtilityRateUpdated(currency_id, util));

        let multiplier_per_block = MultiplierPerBlock::<T>::get().unwrap();
        let base_rate_per_block = BaseRatePerBlock::<T>::get().unwrap();
        let kink = Kink::<T>::get().unwrap();
        let jump_multiplier_per_block = Self::to_decimal(JumpMultiplierPerBlock::<T>::get());

        if util <= kink {
            let rate = util
                .checked_mul(multiplier_per_block)
                .and_then(|r| r.checked_div(DECIMAL))
                .and_then(|r| r.checked_add(base_rate_per_block))
                .unwrap();

            Self::insert_borrow_rate(currency_id, rate);
        } else {
            let normal_rate = kink
                .checked_mul(multiplier_per_block)
                .and_then(|r| r.checked_div(DECIMAL))
                .and_then(|r| r.checked_add(base_rate_per_block))
                .unwrap();

            let excess_util = util.saturating_sub(kink);
            let rate = excess_util
                .checked_mul(jump_multiplier_per_block)
                .and_then(|r| r.checked_add(normal_rate))
                .unwrap();

            Self::insert_borrow_rate(currency_id, rate);
        }
        Ok(())
    }

    pub fn update_supply_rate(
        currency_id: CurrencyId,
        cash: Balance,
        borrows: Balance,
        reserves: Balance,
        reserve_factor_mantissa: Balance,
    ) -> DispatchResult {
        let one_minus_reserve_factor = u128::from(DECIMAL).saturating_sub(reserve_factor_mantissa);

        let borrow_rate = BorrowRate::<T>::get(currency_id);
        let rate_to_pool = Self::to_decimal(borrow_rate.checked_mul(one_minus_reserve_factor));

        let rate = Self::to_decimal(
            Self::utilization_rate(cash, borrows, reserves).checked_mul(rate_to_pool),
        );
        Self::insert_supply_rate(currency_id, rate);

        Ok(())
    }

    pub fn calc_exchange_rate(currency_id: &CurrencyId) -> DispatchResult {
        /*
         *  exchangeRate = (totalCash + totalBorrows - totalReserves) / totalSupply
         */
        let total_position: Position = Self::total_positions(currency_id);
        let total_cash = Self::get_total_cash(currency_id.clone());
        let cash_plus_borrows = total_cash.checked_add(total_position.debit)
            .ok_or(Error::<T>::CalcAccrueInterestFailed)?;
        let exchage_rate = cash_plus_borrows.checked_mul(DECIMAL)
            .and_then(|r| r.checked_div(total_position.collateral)).ok_or(Error::<T>::CalcExchangeRateFailed)?;

        ExchangeRate::<T>::insert(currency_id, exchage_rate);

        Ok(())
    }
}