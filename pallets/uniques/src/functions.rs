// This file is part of Substrate.

// Copyright (C) 2017-2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Various pieces of common functionality.

use super::*;
use frame_support::{ensure, traits::Get, BoundedVec};
use sp_runtime::{DispatchError, DispatchResult};

impl<T: Config<I>, I: 'static> Pallet<T, I> {
  pub(crate) fn do_transfer(
    class: T::ClassId,
    instance: T::InstanceId,
    dest: T::AccountId,
    with_details: impl FnOnce(&ClassDetailsFor<T, I>, &mut InstanceDetailsFor<T, I>) -> DispatchResult,
  ) -> DispatchResult {
    let class_details = Class::<T, I>::get(&class).ok_or(Error::<T, I>::Unknown)?;
    ensure!(!class_details.is_frozen, Error::<T, I>::Frozen);

    let mut details = Asset::<T, I>::get(&class, &instance).ok_or(Error::<T, I>::Unknown)?;
    ensure!(!details.is_frozen, Error::<T, I>::Frozen);
    with_details(&class_details, &mut details)?;

    Account::<T, I>::remove((&details.owner, &class, &instance));
    Account::<T, I>::insert((&dest, &class, &instance), ());
    let origin = details.owner;
    details.owner = dest;
    Asset::<T, I>::insert(&class, &instance, &details);

    Self::deposit_event(Event::Transferred { class, instance, from: origin, to: details.owner });
    Ok(())
  }

  pub(super) fn do_create_class(
    class: T::ClassId,
    owner: T::AccountId,
    admin: T::AccountId,
    deposit: DepositBalanceOf<T, I>,
    free_holding: bool,
    event: Event<T, I>,
  ) -> DispatchResult {
    ensure!(!Class::<T, I>::contains_key(class), Error::<T, I>::InUse);

    T::Currency::reserve(&owner, deposit)?;

    Class::<T, I>::insert(
      class,
      ClassDetails {
        owner: owner.clone(),
        issuer: admin.clone(),
        admin: admin.clone(),
        freezer: admin.clone(),
        total_deposit: deposit,
        free_holding,
        instances: 0,
        instance_metadatas: 0,
        attributes: 0,
        is_frozen: false,
      },
    );

    ClassAccount::<T, I>::insert(&owner, &class, ());
    Self::deposit_event(event);
    Ok(())
  }

  pub(super) fn do_destroy_class(
    class: T::ClassId,
    witness: DestroyWitness,
    maybe_check_owner: Option<T::AccountId>,
  ) -> Result<DestroyWitness, DispatchError> {
    Class::<T, I>::try_mutate_exists(class, |maybe_details| {
      let class_details = maybe_details.take().ok_or(Error::<T, I>::Unknown)?;
      if let Some(check_owner) = maybe_check_owner {
        ensure!(class_details.owner == check_owner, Error::<T, I>::NoPermission);
      }
      ensure!(class_details.instances == witness.instances, Error::<T, I>::BadWitness);
      ensure!(
        class_details.instance_metadatas == witness.instance_metadatas,
        Error::<T, I>::BadWitness
      );
      ensure!(class_details.attributes == witness.attributes, Error::<T, I>::BadWitness);

      for (instance, details) in Asset::<T, I>::drain_prefix(&class) {
        Account::<T, I>::remove((&details.owner, &class, &instance));
      }
      InstanceMetadataOf::<T, I>::remove_prefix(&class, None);
      ClassMetadataOf::<T, I>::remove(&class);
      Attribute::<T, I>::remove_prefix((&class,), None);
      ClassAccount::<T, I>::remove(&class_details.owner, &class);
      T::Currency::unreserve(&class_details.owner, class_details.total_deposit);

      Self::deposit_event(Event::Destroyed { class });

      Ok(DestroyWitness {
        instances: class_details.instances,
        instance_metadatas: class_details.instance_metadatas,
        attributes: class_details.attributes,
      })
    })
  }

  pub(super) fn do_mint(
    class: T::ClassId,
    instance: T::InstanceId,
    owner: T::AccountId,
    with_details: impl FnOnce(&ClassDetailsFor<T, I>) -> DispatchResult,
  ) -> DispatchResult {
    ensure!(!Asset::<T, I>::contains_key(class, instance), Error::<T, I>::AlreadyExists);

    Class::<T, I>::try_mutate(&class, |maybe_class_details| -> DispatchResult {
      let class_details = maybe_class_details.as_mut().ok_or(Error::<T, I>::Unknown)?;

      with_details(&class_details)?;

      let instances = class_details.instances.checked_add(1).ok_or(ArithmeticError::Overflow)?;
      class_details.instances = instances;

      let deposit = match class_details.free_holding {
        true => Zero::zero(),
        false => T::InstanceDeposit::get(),
      };
      T::Currency::reserve(&class_details.owner, deposit)?;
      class_details.total_deposit += deposit;

      let owner = owner.clone();
      Account::<T, I>::insert((&owner, &class, &instance), ());
      let details = InstanceDetails { owner, approved: None, is_frozen: false, deposit };
      Asset::<T, I>::insert(&class, &instance, details);
      Ok(())
    })?;

    Self::deposit_event(Event::Issued { class, instance, owner });
    Ok(())
  }

  pub(super) fn do_burn(
    class: T::ClassId,
    instance: T::InstanceId,
    with_details: impl FnOnce(&ClassDetailsFor<T, I>, &InstanceDetailsFor<T, I>) -> DispatchResult,
  ) -> DispatchResult {
    let owner = Class::<T, I>::try_mutate(
      &class,
      |maybe_class_details| -> Result<T::AccountId, DispatchError> {
        let class_details = maybe_class_details.as_mut().ok_or(Error::<T, I>::Unknown)?;
        let details = Asset::<T, I>::get(&class, &instance).ok_or(Error::<T, I>::Unknown)?;
        with_details(&class_details, &details)?;

        // Return the deposit.
        T::Currency::unreserve(&class_details.owner, details.deposit);
        class_details.total_deposit.saturating_reduce(details.deposit);
        class_details.instances.saturating_dec();
        Ok(details.owner)
      },
    )?;

    Asset::<T, I>::remove(&class, &instance);
    Account::<T, I>::remove((&owner, &class, &instance));

    Self::deposit_event(Event::Burned { class, instance, owner });
    Ok(())
  }

  pub(super) fn do_set_attribute(
    class: T::ClassId,
    maybe_instance: Option<T::InstanceId>,
    maybe_check_owner: &Option<T::AccountId>,
    key: BoundedVec<u8, T::KeyLimit>,
    value: BoundedVec<u8, T::ValueLimit>,
    with_details: impl FnOnce(&ClassDetailsFor<T, I>) -> DispatchResult,
  ) -> DispatchResult {
    let mut class_details = Class::<T, I>::get(&class).ok_or(Error::<T, I>::Unknown)?;
    with_details(&class_details)?;

    let maybe_is_frozen = match maybe_instance {
      None => ClassMetadataOf::<T, I>::get(class).map(|v| v.is_frozen),
      Some(instance) => InstanceMetadataOf::<T, I>::get(class, instance).map(|v| v.is_frozen),
    };
    ensure!(!maybe_is_frozen.unwrap_or(false), Error::<T, I>::Frozen);

    let attribute = Attribute::<T, I>::get((class, maybe_instance, &key));
    if attribute.is_none() {
      class_details.attributes.saturating_inc();
    }
    let old_deposit = attribute.map_or(Zero::zero(), |m| m.1);
    class_details.total_deposit.saturating_reduce(old_deposit);
    let mut deposit = Zero::zero();
    if !class_details.free_holding && maybe_check_owner.is_some() {
      deposit = T::DepositPerByte::get()
        .saturating_mul((key.len().saturating_add(value.len()) as u32).into())
        .saturating_add(T::AttributeDepositBase::get());
    }
    class_details.total_deposit.saturating_accrue(deposit);
    if deposit > old_deposit {
      T::Currency::reserve(&class_details.owner, deposit.saturating_sub(old_deposit))?;
    } else if deposit < old_deposit {
      T::Currency::unreserve(&class_details.owner, old_deposit.saturating_sub(deposit));
    }

    Attribute::<T, I>::insert((&class, maybe_instance, &key), (&value, deposit));
    Class::<T, I>::insert(class, &class_details);

    Self::deposit_event(Event::AttributeSet { class, maybe_instance, key, value });
    Ok(())
  }
}
