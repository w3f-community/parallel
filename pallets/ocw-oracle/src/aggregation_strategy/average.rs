use crate::*;
pub struct Average;

impl<T: Config> AggregationStrategyApi<T> for Average {
    fn aggregate_price(
        round_index: &RoundIndex<T::BlockNumber>,
        provider: &Vec<T::AccountId>,
        currency_id: &CurrencyId,
    ) -> Result<PriceDetail, Error<T>> {
        let mut prices = vec![];
        provider.iter().for_each(|account_id| {
            Pallet::<T>::ocw_oracle_data_source().iter().for_each(
                |data_source_enum: &DataSourceEnum| {
                    let ovp: Option<VecDeque<PriceDetailOf<T::BlockNumber>>> =
                        Pallet::<T>::ocw_oracle_price(account_id, (data_source_enum, currency_id));
                    ovp.and_then(|vp| -> Option<()> {
                        vp.back().and_then(|p| -> Option<()> {
                            if round_index == &p.index {
                                prices.push(p.price);
                            } else {
                                log::warn!(
                                    "price round index is {:?}, while this round is {:?}",
                                    p.index,
                                    round_index
                                );
                            }
                            Some(())
                        })
                    });
                },
            );
        });
        let avg = prices.iter().sum::<u128>() / (prices.len() as u128);
        let now = T::Time::now();
        let timestamp: Timestamp = now.try_into().or(Err(Error::<T>::ParseTimestampError))?;
        Ok((avg, timestamp))
    }
}
