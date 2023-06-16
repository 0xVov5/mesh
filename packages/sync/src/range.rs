use std::{
    iter::Sum,
    ops::{Add, Sub},
};
use thiserror::Error;

use cosmwasm_schema::cw_serde;

/// This is designed to work with two numeric primitives that can be added, subtracted, and compared.
#[cw_serde]
#[derive(Default, Copy)]
pub struct ValueRange<T>(T, T);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RangeError {
    #[error("Underflow minimum value")]
    Underflow,
    #[error("Overflow maximum value")]
    Overflow,
}

impl<T> ValueRange<T>
where
    T: Copy,
{
    pub fn new(value: T) -> Self {
        Self(value, value)
    }

    pub fn min(&self) -> T {
        self.0
    }

    pub fn max(&self) -> T {
        self.1
    }
}

pub fn max_val<'a, I, T>(iter: I) -> T
where
    I: Iterator<Item = &'a ValueRange<T>> + 'a,
    T: Ord + Copy + Default + 'a,
{
    iter.map(|r| r.max()).max().unwrap_or_default()
}

pub fn min_val<'a, I, T>(iter: I) -> T
where
    I: Iterator<Item = &'a ValueRange<T>> + 'a,
    T: Ord + Copy + Default + 'a,
{
    iter.map(|r| r.min()).min().unwrap_or_default()
}

/// Captures the spread from the lowest low to the highest high
pub fn spread<I, T>(iter: I) -> ValueRange<T>
where
    I: Iterator<Item = ValueRange<T>>,
    T: Ord + Copy + Default,
{
    iter.reduce(|acc, x| {
        ValueRange(
            std::cmp::min(acc.min(), x.min()),
            std::cmp::max(acc.max(), x.max()),
        )
    })
    .unwrap_or_default()
}

impl<T: Ord> ValueRange<T> {
    pub fn contains(&self, value: T) -> bool {
        self.0 <= value && value <= self.1
    }
}

impl<T> ValueRange<T>
where
    T: Add<Output = T> + Sub<Output = T> + Ord + Copy,
{
    /// This is a check for calling code if it wishes to change the (externally defined) maximum for the range.
    /// Usage is eg modifying collateral while the range is total liens on the collateral
    pub fn valid_max(&self, new_max: T) -> bool {
        self.1 <= new_max
    }

    /// This is a check for calling code if it wishes to change the (externally defined) minimum for the range.
    pub fn valid_min(&self, new_min: T) -> bool {
        self.0 >= new_min
    }

    /// This is to be called at the beginning of a transaction, to reserve the ability to commit (or rollback) an addition.
    /// It doesn't enforce any maximum value. Use `prepare_add_max` for that.
    pub fn prepare_add(&mut self, value: T) -> Result<(), RangeError> {
        self.1 = self.1 + value;
        Ok(())
    }

    /// This should be used instead of prepare_add if we wish to enforce a maximum value
    pub fn prepare_add_max(&mut self, value: T, max: T) -> Result<(), RangeError> {
        if self.1 + value > max {
            return Err(RangeError::Overflow);
        }
        self.1 = self.1 + value;
        Ok(())
    }

    /// The caller should limit these to only previous `prepare_add` calls.
    /// We will panic on mistake as this should never happen
    pub fn rollback_add(&mut self, value: T) {
        self.1 = self.1 - value;
        self.assert_valid_range();
    }

    /// The caller should limit these to only previous `prepare_add` calls.
    /// We will panic on mistake as this should never happen
    pub fn commit_add(&mut self, value: T) {
        self.0 = self.0 + value;
        self.assert_valid_range();
    }

    /// This is to be called at the beginning of a transaction, to reserve the ability to commit (or rollback) a subtraction.
    /// It assumes we are enforcing a minimum value of 0. If you want a different minimum, use `prepare_sub_min`
    pub fn prepare_sub(&mut self, value: T) -> Result<(), RangeError> {
        if self.0 < value {
            return Err(RangeError::Underflow);
        }
        self.0 = self.0 - value;
        Ok(())
    }

    /// This is to be called at the beginning of a transaction, to reserve the ability to commit (or rollback) a subtraction.
    /// You can specify a minimum value that the range must never go below. If you pass `None`, it will not even enforce
    /// a minimum of 0.
    pub fn prepare_sub_min(
        &mut self,
        value: T,
        min: impl Into<Option<T>>,
    ) -> Result<(), RangeError> {
        if let Some(min) = min.into() {
            // use plus not minus here, as we are much more likely to have underflow on u64 or Uint128 than overflow
            if self.0 < min + value {
                return Err(RangeError::Underflow);
            }
        }
        self.0 = self.0 - value;
        Ok(())
    }

    /// The caller should limit these to only previous `prepare_sub` calls.
    /// We will panic on mistake as this should never happen
    pub fn rollback_sub(&mut self, value: T) {
        self.0 = self.0 + value;
        self.assert_valid_range();
    }

    /// The caller should limit these to only previous `prepare_sub` calls.
    /// We will panic on mistake as this should never happen
    pub fn commit_sub(&mut self, value: T) {
        self.1 = self.1 - value;
        self.assert_valid_range();
    }

    #[inline]
    fn assert_valid_range(&self) {
        assert!(self.0 <= self.1);
    }
}

impl<T: Add<Output = T>> Add for ValueRange<T> {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        ValueRange(self.0 + rhs.0, self.1 + rhs.1)
    }
}

impl<T: Add<Output = T> + Default> Sum for ValueRange<T> {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(ValueRange::default(), |acc, x| acc + x)
    }
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::Uint128;

    use super::*;

    #[test]
    fn comparisons() {
        // check for one point - it behaves like an integer
        let mut range = ValueRange::new(50);
        // valid_min + valid_max is like equals
        assert!(range.valid_max(50));
        assert!(range.valid_min(50));
        // valid_max + !valid_min is >=
        assert!(range.valid_max(51));
        assert!(!range.valid_min(51));
        // valid_min + !valid_max is <=
        assert!(!range.valid_max(49));
        assert!(range.valid_min(49));

        // make a range (50, 80), it should compare properly to those outside the range
        range.prepare_add(30).unwrap();
        assert!(!range.valid_max(49));
        assert!(range.valid_min(49));
        assert!(range.valid_max(81));
        assert!(!range.valid_min(81));

        // all comparisons inside the range lead to false
        assert!(!range.valid_max(60));
        assert!(!range.valid_min(60));
    }

    #[test]
    fn add_ranges() {
        // (80, 120)
        let mut range = ValueRange::new(80);
        range.prepare_add(40).unwrap();

        // (100, 200)
        let mut other = ValueRange::new(200);
        other.prepare_sub(100).unwrap();

        let total = range + other;
        assert_eq!(total, ValueRange(180, 320));
    }

    #[test]
    fn sums() {
        let ranges = [
            ValueRange::new(100),
            ValueRange(0, 250),
            ValueRange::new(200),
            ValueRange(170, 380),
        ];
        let total: ValueRange<u32> = ranges.into_iter().sum();
        assert_eq!(total, ValueRange(470, 930));
    }

    #[test]
    fn min_max() {
        let ranges = [
            ValueRange::new(100),
            ValueRange(40, 250),
            ValueRange::new(200),
            ValueRange(170, 380),
        ];
        let max = max_val(ranges.iter());
        assert_eq!(max, 380);

        let min = min_val(ranges.iter());
        assert_eq!(min, 40);

        let all = spread(ranges.into_iter());
        assert_eq!(all, ValueRange(40, 380));
    }

    // most tests will use i32 for simplicity - just ensure APIs work properly with Uint128
    #[test]
    fn works_with_uint128() {
        // (80, 120)
        let mut range = ValueRange::new(Uint128::new(80));
        range.prepare_add(Uint128::new(40)).unwrap();

        // (100, 200)
        let mut other = ValueRange::new(Uint128::new(200));
        other.prepare_sub(Uint128::new(100)).unwrap();

        let total = range + other;
        assert_eq!(total, ValueRange(Uint128::new(180), Uint128::new(320)));
    }

    // This test attempts to use the API in a realistic scenario.
    // A user has X collateral and makes some liens on this collateral, which execute asynchronously.
    // That is, we want to process other transactions while the liens are being executed, while ensuring there
    // will not be a conflict on rollback or commit.
    //
    // using u64 not Uint128 here as less verbose
    #[test]
    fn real_world_usage() {
        let mut collateral = 10_000u64;
        let mut lien = ValueRange::new(0u64);

        // prepare some lien
        lien.prepare_add_max(2_000, collateral).unwrap();
        lien.prepare_add_max(5_000, collateral).unwrap();

        // cannot add too much
        let err = lien.prepare_add_max(3_500, collateral).unwrap_err();
        assert_eq!(err, RangeError::Overflow);

        // let's commit the second pending lien (only 2000 left)
        // QUESTION: should we enforce the min/max on commit/rollback explicitly and pass them in?
        lien.commit_add(5_000);
        assert_eq!(lien, ValueRange(5_000, 7_000));

        // See we cannot reduce this by 4_000
        assert!(!lien.valid_max(collateral - 4_000));
        // See we can reduce this by 2_000
        assert!(lien.valid_max(collateral - 2_000));
        collateral -= 2_000;

        // start unbonding 3_000
        lien.prepare_sub(3_000).unwrap();
        // still; cannot increase max (7_000) over the new cap of 8_000
        let err = lien.prepare_add_max(1_500, collateral).unwrap_err();
        assert_eq!(err, RangeError::Overflow);

        // if we rollback the other pending lien, this works
        lien.rollback_add(2_000);
        assert_eq!(lien, ValueRange(2_000, 5_000));
        lien.prepare_add_max(1_500, collateral).unwrap();
    }
}
