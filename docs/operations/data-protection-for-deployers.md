# Data-protection notice and checklist for deployers

This document is operational guidance for self-hosting organisations. Replace bracketed fields and obtain jurisdiction-specific review.

## Model notice

**Controller:** [organisation and contact]  
**Data-protection contact:** [contact]  
**System:** Teslatlas Hub vehicle telemetry platform

We process vehicle and user data to [state purposes]. Data may include account and vehicle identifiers, precise location, journeys, timestamps, charging records, diagnostic state, network information, audit logs and user-selected labels.

Our legal bases are [identify each purpose and basis]. Where required, we obtain consent before collecting data or associating a person with a vehicle.

Data is collected from [vehicle/API/import/user], stored at [locations], accessed by [roles], disclosed to [recipients] and retained for [period/rule]. International transfers are [describe safeguards].

Individuals may exercise applicable rights by contacting [contact]. They may withdraw consent where processing relies on consent. Complaints may be made to [supervisory authority].

Automated decision-making: [none/describe].  
Security: [summary].  
Last updated: [date].

## Deployment checklist

### Scope and people

- identify owners, drivers, employees, family members and passengers affected;
- separate personal, household, employment and fleet contexts;
- document controller/processor roles;
- consult worker representatives where required.

### Necessity and lawful basis

- define each purpose;
- remove data not needed for that purpose;
- do not bundle consent;
- record withdrawal and revocation;
- do not repurpose historical location silently.

### Access and security

- role-based access and MFA where available;
- dedicated service and database accounts;
- encryption in transit and at rest where proportionate;
- secret rotation;
- audit logging without secret leakage;
- patching, backups and restoration tests;
- documented breach response.

### Retention and rights

- configure retention by data category;
- test deletion, export, correction and restriction;
- cover backups and derived exports;
- log legal holds;
- identify who answers requests.

### Suppliers and transfers

- inventory hosting, DNS, email, crash, map and support providers;
- execute processor terms where required;
- record subprocessors;
- assess international transfer mechanisms;
- verify provider retention and training use.

### High-risk processing

Complete a DPIA before large-scale tracking, employee monitoring, systematic location profiling, combining multiple data sources, or processing that could create material physical or economic harm.

## Software limitations

Teslatlas Hub provides technical controls but does not select the deployer's lawful basis, write its final privacy notice, obtain employment consent or answer data-subject requests automatically.
