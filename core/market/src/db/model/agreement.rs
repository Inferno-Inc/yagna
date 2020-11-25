use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use diesel::sql_types::Text;
use serde::{Deserialize, Serialize};

use ya_client::model::market::agreement::{
    Agreement as ClientAgreement, State as ClientAgreementState,
};
use ya_client::model::market::demand::Demand as ClientDemand;
use ya_client::model::market::offer::Offer as ClientOffer;
use ya_client::model::{ErrorMessage, NodeId};
use ya_diesel_utils::DbTextField;

use crate::db::model::{OwnerType, Proposal, ProposalId, SubscriptionId};
use crate::db::schema::market_agreement;

pub type AgreementId = ProposalId;
pub type AppSessionId = Option<String>;

/// TODO: Could we avoid having separate enum type for database
///  and separate for client?
#[derive(
    strum_macros::EnumString,
    DbTextField,
    derive_more::Display,
    AsExpression,
    FromSqlRow,
    PartialEq,
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
)]
#[sql_type = "Text"]
pub enum AgreementState {
    /// Newly created by a Requestor (based on Proposal)
    Proposal,
    /// Confirmed by a Requestor and sent to Provider for approval
    Pending,
    /// Cancelled by a Requestor
    Cancelled,
    /// Rejected by a Provider
    Rejected,
    /// Approved by both sides
    Approved,
    /// Not accepted, rejected nor cancelled within validity period
    Expired,
    /// Finished after approval
    Terminated,
}

#[derive(Clone, Debug, Identifiable, Insertable, Queryable, Serialize, Deserialize)]
#[table_name = "market_agreement"]
pub struct Agreement {
    pub id: AgreementId,

    pub offer_properties: String,
    pub offer_constraints: String,

    pub demand_properties: String,
    pub demand_constraints: String,

    pub offer_id: SubscriptionId,
    pub demand_id: SubscriptionId,

    pub offer_proposal_id: ProposalId,
    pub demand_proposal_id: ProposalId,

    pub provider_id: NodeId,
    pub requestor_id: NodeId,

    pub session_id: AppSessionId,

    /// End of validity period.
    /// Agreement needs to be accepted, rejected or cancelled before this date; otherwise will expire.
    pub creation_ts: NaiveDateTime,
    pub valid_to: NaiveDateTime,

    pub approved_ts: Option<NaiveDateTime>,
    pub state: AgreementState,

    pub proposed_signature: Option<String>,
    pub approved_signature: Option<String>,
    pub committed_signature: Option<String>,
}

impl Agreement {
    pub fn new(
        demand_proposal: Proposal,
        offer_proposal: Proposal,
        valid_to: NaiveDateTime,
        owner: OwnerType,
    ) -> Agreement {
        let creation_ts = Utc::now().naive_utc();
        Agreement::new_with_ts(
            demand_proposal,
            offer_proposal,
            valid_to,
            creation_ts,
            owner,
        )
    }

    pub fn new_with_ts(
        demand_proposal: Proposal,
        offer_proposal: Proposal,
        valid_to: NaiveDateTime,
        creation_ts: NaiveDateTime,
        owner: OwnerType,
    ) -> Agreement {
        let agreement_id = ProposalId::generate_id(
            &offer_proposal.negotiation.offer_id,
            &offer_proposal.negotiation.demand_id,
            &creation_ts,
            owner,
        );

        Agreement {
            id: agreement_id,
            offer_properties: offer_proposal.body.properties,
            offer_constraints: offer_proposal.body.constraints,
            demand_properties: demand_proposal.body.properties,
            demand_constraints: demand_proposal.body.constraints,
            offer_id: offer_proposal.negotiation.offer_id,
            demand_id: demand_proposal.negotiation.demand_id,
            offer_proposal_id: offer_proposal.body.id,
            demand_proposal_id: demand_proposal.body.id,
            provider_id: offer_proposal.negotiation.provider_id, // TODO: should be == demand_proposal.negotiation.provider_id
            requestor_id: demand_proposal.negotiation.requestor_id,
            session_id: None,
            creation_ts,
            valid_to,
            approved_ts: None,
            state: AgreementState::Proposal,
            proposed_signature: None,
            approved_signature: None,
            committed_signature: None,
        }
    }

    pub fn into_client(self) -> Result<ClientAgreement, ErrorMessage> {
        let demand_properties = serde_json::from_str(&self.demand_properties)
            .map_err(|e| format!("Can't serialize Demand properties. Error: {}", e))?;
        let offer_properties = serde_json::from_str(&self.offer_properties)
            .map_err(|e| format!("Can't serialize Offer properties. Error: {}", e))?;

        let demand = ClientDemand {
            properties: demand_properties,
            constraints: self.demand_constraints,
            requestor_id: self.requestor_id,
            demand_id: self.demand_id.to_string(),
            timestamp: Utc.from_utc_datetime(&self.creation_ts),
        };
        let offer = ClientOffer {
            properties: offer_properties,
            constraints: self.offer_constraints,
            provider_id: self.provider_id,
            offer_id: self.offer_id.to_string(),
            timestamp: Utc.from_utc_datetime(&self.creation_ts),
        };
        Ok(ClientAgreement {
            agreement_id: self.id.into_client(),
            demand,
            offer,
            valid_to: DateTime::<Utc>::from_utc(self.valid_to, Utc),
            approved_date: self.approved_ts.map(|d| DateTime::<Utc>::from_utc(d, Utc)),
            state: self.state.into(),
            timestamp: Utc.from_utc_datetime(&self.creation_ts),
            app_session_id: None,
            proposed_signature: self.proposed_signature,
            approved_signature: self.approved_signature,
            committed_signature: self.committed_signature,
        })
    }
}

impl From<AgreementState> for ClientAgreementState {
    fn from(agreement_state: AgreementState) -> Self {
        match agreement_state {
            AgreementState::Proposal => ClientAgreementState::Proposal,
            AgreementState::Pending => ClientAgreementState::Pending,
            AgreementState::Cancelled => ClientAgreementState::Cancelled,
            AgreementState::Rejected => ClientAgreementState::Rejected,
            AgreementState::Approved => ClientAgreementState::Approved,
            AgreementState::Expired => ClientAgreementState::Expired,
            AgreementState::Terminated => ClientAgreementState::Terminated,
        }
    }
}