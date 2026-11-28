# Bruce's Database: foundations

This file records the decisions which bruce-db's foundations rest on, as a store for many owners and many apps, not
only for Bruce's Smart Home. They were reached in a design discussion on 27 November 2026, prompted by building
Security Contexts (step 12) and finding that the app's data model and bruce-db's were not the same thing.

Nothing here is built yet. Where a decision changes [design.md](design.md), or what is built, it says so, under
[What changes](#what-changes); design.md is to be updated to agree once these are accepted, and stays the master copy
of the detail.

## Summary

- **A domain is an administrative realm,** independent of every other, a person's, a household's or an
  organisation's, across every app it uses, whose Owners together have absolute control of its data.
- **Every Entity, a single record, is identified by the hash of its Identity Object.** A domain and its first Owner
  are made together, and every Principal is contained in exactly one domain.
- **The Owners together have absolute control of their domain,** an immutable Rule: the root SC is Admin of every
  other SC, through each SC's parent, and the Owners are its Admins. Policies are the users' own, and bind the Owners
  too: if they paint themselves into a corner, that is their choice.
- **A principal is a who, not an app.** Bruce's user principal is Bruce in every app he uses; an app's installation,
  or a capture device, has a system principal of its own, and acts for a user by delegation.
- **An SC is keys and members, and nothing else.** bruce-db has no idea of an app.
- **Admin, Write and Read are independent permissions,** not a ladder: owners have power over everything, not
  necessarily sight of it.
- **Controls are ACLs within a domain, enforced by its trusted nodes, and cryptography between domains,** or wherever
  data sits on a node the domain does not trust. Everything is signed; encryption is chosen per SC.
- **Policies are records in the domain:** thresholds of approvals, Chinese Walls between SC graphs, and how the
  policies themselves change.
- **One store per node, one writer,** whatever apps use it, every read filtered by the requester's SC memberships, a
  replication feed and a query alike. bruce-db runs embedded on a device, embedded and shared through file locks on a
  user's computer, or as a service on files or DynamoDB, with one API.
- **Content which crosses domains is copied,** and each store's owner decides how long what it holds is kept.
- **Only bruce-db's own types are kinds in hash keys;** an app's Sequences are opaque IDs, hashed from a descriptor
  naming a kind which is itself a signed schema record.
- **A Sequence is what is stored, a DataSet what an app works with,** e.g. one month of a meter's readings.

## Terms

- **Entity:** a single record, an atomic item in the database, in JSON. Every Entity is either a **System Entity**,
  one of bruce-db's own, e.g. a Principal, an SC, a membership or a DirectoryInfo, or an application's.
- **Identity Object:** the JSON object which defines an Entity. Every Entity is identified by the hash of its Identity
  Object, its ID, so that anyone can check an ID against its object, and no one can make another object with the same
  ID. An Identity Object is always canonical JSON, made canonical as part of making the ID; the rest of an Entity need
  not be.
- **Object:** an Entity contained in an SC. Every application Entity is an Object, and has a `payload` attribute, the
  application's own data, expected but not required to be JSON. Some System Entities are Objects, e.g. a Principal's
  DirectoryInfo, in an SC; others are in no SC, e.g. Principal, PrincipalKey (the public part of a Principal's key),
  WrappedKey (the secret part of a Principal's key, wrapped), SecurityContext and SecurityContextMember. The rest will
  be settled as they are built.
- **Absolute Hash:** an Object's ID, the hash of its Identity Object. Something simple which never changes, e.g. a
  meter reading, is its own Identity Object, so that every Object is stored canonical. An Object which changes has a
  new Absolute Hash for each version, the hash of its new value.
- **An Object's Identity Object holds what it is about,** e.g. a meter reading's meter and the period it covers, which
  is in effect its Sequence and its sort key, so that two meters' equal readings have different hashes. On DynamoDB
  the whole Identity Object is stored, as an item is charged by the KB and most are well under one; in a file store it
  is not stored twice, but made again from the Sequence, the sort key and the value when its hash is wanted.
- **Relative Hash:** the Absolute Hash of an Object's first version, which every later version carries, so that all
  the versions of an Object share it. Two indexes follow, needed only by a store with very much data, e.g. on
  DynamoDB, not by the file store now: Absolute Hash to hash key, to fetch any version by its hash; and each Object's
  versions as a Sequence by Relative Hash, whose last entry is the latest version. On DynamoDB they are kept by a
  Lambda reading the table's Stream, rather than in a transaction, as idempotent, eventual completeness is all that is
  needed at scale; the details are for when it is built.
- **Sequence:** the database's structure for a collection of Objects, often but not necessarily of the same type: one
  hash key, its Objects ordered by sort key, e.g. Smart Home's MeterReadings, all of a meter's readings. Every Sequence
  has a type, which says how its Objects are grouped into DataSets:
  - **SingleRecord:** one Object, e.g. an account;
  - **List:** Objects in the order they were first given;
  - **OrderedList:** Objects ordered by a key of their own, e.g. bills;
  - **Monthly:** Objects which grow in a way not known in advance, a month at a time;
  - **MonthlyFixedSlot:** Objects in contiguous slots, e.g. every half hour, a month at a time, which need not arrive
    in order, e.g. MeterReadings;
  - **MultiQuery:** the replies to several queries, each by its place;

  and more as they are needed.
- **DataSet:** the part of a Sequence which an application works with as a unit, in code and in memory, e.g. one
  month of MeterReadings, a `MonthlyFixedSlotDataSet`, as Smart Home's `src/data_set/` has them. For a SingleRecord or
  a List, the DataSet is the whole Sequence.
- **Domain:** an administrative realm, independent of every other domain. It has one or more **Owners**, Principals
  who together have absolute control over all the data in it: power over it, not necessarily sight of it, see 4.
- **Principal:** an actor in the system, contained in exactly one domain:
  - a **User Principal** represents a person who uses the system;
  - a **System Principal** represents an actor which is not human, e.g. a process, an app's installation or a capture
    device.

  A Principal has a set of keys, and its Identity Object holds its domain's ID and the first of its keys. It shows
  that it is that Principal by signing with a private key of its set. A Principal is in no SC.
- **Founding:** a domain and its first Owner are made in one operation. The domain's Identity Object holds the first
  Owner's public keys, which are new, so that they are enough to make its ID unique; its ID is then put in the
  Owner's Identity Object, so that the first Owner is contained in the domain as every other Principal is. This is how
  step 12 builds it.
- **Actor and Subject:** an operation is done by its **Actor**, the Principal executing it, and may be done on behalf
  of a **Subject**, the Principal who authorised it. A System Principal may act autonomously, as itself, e.g. a
  capture device replicating its data to the others, or on behalf of another Principal. **On behalf of** means that
  the Subject approved the operation, by asking for it, not that they authenticated for it: they need not have typed
  their password for each operation, but they explicitly requested it.
- **Security Context (SC):** the unit of sharing. Every Object is in exactly one SC, and may be encrypted with the
  SC's key. Membership of an SC grants Read, Write and Admin independently, of its Objects and of the SC itself. A
  member is a Principal or another SC.
- **Root SC:** a domain's special SC, whose Admin members are the domain's Owners. Every other SC has a **parent**, an
  SC which is a member of it with Admin, so that the root is, directly or through parents, Admin of every SC in the
  domain, and the Owners can reach every SC with Admin.
- **SC Graph:** all the SCs reachable from an initial SC, through the SCs it is a member of, and theirs: a sub-graph
  of the domain's SCs, which form a directed graph, not a tree.
- **Rule:** what bruce-db makes true and no one can change: the Owners together have absolute control of their
  domain; they are the root SC's Admins; every SC but the root has a parent with Admin, so that the root reaches
  every SC with Admin; every Object is in exactly one SC.
- **Policy:** a limit the domain's users place on it, which binds everyone, Owners too, e.g. a threshold of approvals,
  or a **Chinese Wall**, a relationship between two SC graphs which
  requires that no Principal be a member of an SC in each.
- **Node:** one store and its one writer: a capture device, a user's computer, or a service.
- **Store:** what a node holds: part or all of its own domain, and copies of other domains' Entities.
- **App:** a program which is a client of a node, through its installation's System Principal. It is not a concept in
  bruce-db.
- **Kind:** a schema, an Entity signed by its author, which says what a Sequence's Objects mean. Several apps may share
  one.
- **Site:** Bruce's Smart Home's own concept, one household's installation, a house, a test copy or an offline copy,
  within its owner's domain. It is what the app's profiles were, and is no concern of bruce-db's.

## Decisions

### 1. A domain is an owner

- **A domain is one owner's tenancy:** a person, a household or an organisation, and everything it owns and controls,
  in every app. It is the unit of ownership, of policy, of backup, export and deletion, and, on DynamoDB, of
  partitioning, its ID first in every hash key.
- **It is not an app's data, nor a site.** The name Bruce chose for his first domain, "7 Sadlers Close", is a place,
  which showed that Smart Home's profile is not a domain: Bruce's house, its test copy and its offline copies are all
  his, so all are in his domain, as Smart Home's sites.
- **A domain starts with its first Owner,** even a multinational's: the two are made together, see Founding, and the
  Owner is contained in the domain as every later Principal is. More Principals follow, some of them Owners.
- **Someone from another domain is a reader, not an owner.** Jeff may be a member of an SC in Bruce's domain, but his
  principal stays in his own, and Bruce's domain holds only the membership which lets him read.

### 2. A principal is a who, not an app

- **A user principal is a person in every app.** Bruce has one user principal in his domain, used by every app he
  runs, from one key file.
- **A system principal is a thing:** an app's installation, or a capture device, one for each installation or device
  in each domain it serves. It acts as itself, e.g. when it polls, and for a user by delegation, with the user as the
  subject and itself as the actor, as signing already does.
- **There may be many of either,** and neither implies the other.

### 3. An SC is keys and members

- **An SC controls access, and does nothing else.** It has members, each with permissions, and, if encrypted, keys.
  It is not an app, and bruce-db neither knows nor checks which app uses which SC.
- **An app may keep to SCs of its own,** which is convenient, e.g. Smart Home's `control` and `data`, but nothing
  requires it. The app SC built in step 12, between the domain and those two, is not needed.
- **Every SC but the root has a parent,** a member of it with Admin, so that every SC is reachable with Admin from the
  root, whose Admins are the Owners. An SC may have other members which are SCs too, so that the SCs form a directed
  graph, loops allowed, as the design has it.

### 4. Admin, Write and Read are independent

- **A membership grants a set of permissions,** any of Admin (manage the SC's members), Write (put its records) and
  Read (read them), not a ladder in which Admin includes Write and Write includes Read.
- **Through the SC graph,** a path gives the intersection of the permissions along it, and a principal has the union
  of what its paths give. Going round a loop can only take permissions away, so loops stay harmless.
- **Owners have power, not visibility.** Through the root SC they are Admin of every SC, a Rule which no policy can
  change: no SC can be taken out of the Owners' reach. Policies may still govern how they use it, e.g. how many must
  agree, see 6. But Admin is not Read, and reading an SC needs Read in it. An Owner who needs to see an SC must be given
  Read, which is a membership event, signed, kept in its history and checked against the domain's Chinese Walls, which
  stop an Owner, as any Principal, having Read or Write on both sides of one: breaking the glass is visible, not a
  privilege nobody notices.
- **Write without Read** is useful: a capture device puts records but need not read the household's other data. For an
  encrypted SC, writing needs only its public key.

### 5. ACLs within a domain, cryptography between domains

- **Within a domain, controls are ACLs,** enforced by its trusted nodes: a server which none of the users controls,
  run by its operator, as an organisation's IT department or cloud provider is today, or, for a household, its
  capture devices and its own computers. The node checks each request's signature and the requester's permissions,
  and refuses what is not allowed. This is how "power, not visibility" is enforced, with no cryptography beyond
  signatures.
- **Between domains, and on nodes a domain does not trust,** e.g. another organisation's, someone else's cloud, or a
  backup, the only protection is cryptography: records encrypted for an SC's members and signed by their author.
- **Everything is signed,** as it is cheap, and it is how a node knows who is asking, how a copy proves who wrote it,
  and how another domain checks a record.
- **Encryption is chosen per SC.** One shared with another domain, or kept where the domain does not trust the node,
  is encrypted; one kept only on the domain's trusted nodes need not be, or is encrypted at rest with keys its nodes
  hold. Keys wrapped for members, in their membership records, are needed only for encrypted SCs.
- **There are two kinds of server:** a trusted one, which enforces ACLs, and an untrusted one, which stores and
  replicates ciphertext it cannot read, checking only signatures. A domain may use both.
- **A user's own computer is one trust zone.** Any process running as that user can read the store's files, so
  filtering between that user's apps there is a convenience, not a control; it is enforced at the boundaries, between
  nodes.

### 6. Policies are records in the domain

- **Policies are signed records in the domain,** limits on its SCs, evaluated by every node, in signing-time order,
  against the policies in force when each event was signed, as membership's validity already is. An action without
  the approvals its policy needs is invalid everywhere.
- **A small, fixed set of policy kinds,** not a general language, as every node evaluates them, a device too:
  - **thresholds:** for each kind of action, who must approve and how many, e.g. all active owners to delete the
    domain, all the other owners to eject an owner, or two of the security officers to give someone Read in HR's SC.
    An action collects signatures until it meets its threshold;
  - **Chinese Walls:** a relationship between two SC graphs, such that no Principal may be a member of an SC in each.
    The Owners' Admin of every SC is not affected, but no Owner may get Read or Write on SCs on both sides. Membership
    history is kept, so "never" can mean ever: someone once in one may not join the other after leaving it, as
    knowledge cannot be taken back, only access.
- **Policies are the users', not the system's,** and bind everyone, Owners too. The Rules about Owners, see Terms,
  are not policies, and cannot be changed.
- **The Owners keep control only if their policies let them,** which is their choice and their problem. E.g. "all
  Owners must agree to delete the domain" and "all remaining Owners must agree to eject an Owner" keep it as long as
  only one Owner becomes unavailable at a time, e.g. dies or leaves the company: the others eject them first. In a
  domain of two, either may eject the other, then delete the domain. Owners would usually choose policies which
  cannot leave them without control, e.g. a backup Owner, kept offline, but nothing makes them.
- **A policy says what is needed to change it,** chosen by whoever makes it, so that "unanimous to delete" means
  something: if one Owner could change it, it would not.
- **The defaults are a household's:** one owner, or any owner acting alone, which is how Smart Home works now.
- **The system limits what it grants, not what people do:** a member can always pass on what they have read.

### 7. One store per node

- **A node has one store, and one writer,** whatever apps use it. Every read is filtered by the requester's
  memberships of the SCs its Sequences are in: a replication feed and an interactive query are the same, with
  different cursors, so replication does not depend on any app.
- **Three ways of running bruce-db,** one engine and one API, so that an app does not know which it has:
  - **embedded, alone,** on a capture device: one process, the firmware its only client, as now;
  - **embedded and shared,** on a single user's computer: the library in each app, sharing one store through file
    locks, each held for a write, not for the life of the process, as SQLite does;
  - **a service,** the same engine behind the API, on files or DynamoDB, for several users or a server, which on
    DynamoDB has many stateless writers, using conditional writes.
- **The feed is ordered lanes,** see [The feed](#the-feed), one for each writer on a node, one for each SC and writer
  on a service, so that nothing in it is a single sequence for a whole domain.
- **The file lock stays as a guard rail** against a second copy of an app, as now.
- **The store belongs to the user, not an app,** once several apps share it: it moves from an app's data directory to
  a bruce-db location, e.g. `~/Library/Application Support/bruce-db/`, and each app keeps only its own settings.
- **Sharing a store between processes needs:**
  - a cheap marker of what has changed, so that each process, before reading or when it takes the lock, reads only
    what others have written since it last looked;
  - journal entries which are all there or plainly not, checked, so that a process which dies mid-write breaks
    nothing; `flock` is released by the system when it dies;
  - compaction under the write lock, writing new files and renaming them into place, which a reader on macOS keeps
    reading through its open handle; Windows will not replace an open file, so needs another way, e.g. keeping old
    files until no reader has them open;
  - a quick open, as each app opens the store itself.

#### The feed

- **A cursor is safe only if, in each stream, numbers appear in the order they become visible,** so every stream
  has exactly one sequencer, and the feed scales by having many streams, lanes, as Kafka has partitions. A cursor is
  one number for each lane. There is no design with neither a single sequencer nor a cursor which grows; the choice
  is only what the lanes are.
- **A number given before its write commits is not safe,** as a counter for an SC would be: given 4 to A and 5 to
  B, B's write may land first, a reader read up to 5, and A's 4 land after, never seen. A time has the same race,
  which is why the design does not use times for the feed.
- **Replication needs no total order,** as the newest version wins and puts are idempotent: a cursor need only
  ensure that nothing is missed, which ordering within each lane gives.
- **A file store's feed is one lane,** its single writer's sequence, `n`, as built, which covers every SC it holds
  and records replicated from elsewhere, as each is given that node's own number when stored. Filtering by SC skips
  what a reader may not see, but its cursor is still the one number. So **a node keeps one number for each peer it
  pulls from.**
- **On a service, the lanes are SC and writer:** the feed's index has partition key `(SC, writer)`, sorted by the
  writer's `n`, the design's index by domain and writer with the SC added, so that a reader queries only the SCs it
  may read, and in each only the writers which have written to it. Its cost is in proportion to what it may see, not
  to the domain.
- **A writer puts its changes in the order of its sequence,** never overlapping puts which could land out of order,
  which the one-writer rule makes natural, or a reader passing a later number would miss an earlier one.
- **Each SC has a summary partition,** a row for each writer, holding that writer's latest `n`, which each writer
  updates in its own row only, so that writers never contend. One Query gives the SC's state, and a reader then
  queries only the lanes which have moved.
- **A cursor is the reader's,** under the stateless-server principle: a number for each writer of each SC it follows.
  A writer keeps one number, the last it put; the service keeps none.
- **How many,** for a reader of a service:

  | Reader | SCs followed | Writers in each | Cursor entries |
  | --- | --- | --- | --- |
  | A household's node | 3 | about 6 | about 18 at most |
  | A member of a small team | 20 | 10 | 200 |
  | An employee of a large organisation | 200 | 50 active | 10,000, about 200 KB |

  Each pull queries each followed SC's summary, 200 for the employee, then the lanes which moved. The number of SCs,
  not of writers, is the cost, which notifications of which SCs have moved, planned, cut to those which changed.
- **Writers which have gone quiet drop out** of a cursor, once the reader is past their last `n`, and come back if
  the summary shows them writing again.
- **An SC with thousands of active writers,** e.g. a whole company's channel, is where lanes for each writer stop
  working, and where one of two other kinds of lane would cap it, designed only when such an SC exists, see Open
  questions:
  - **stream shards:** a Lambda for each shard of the table's DynamoDB Stream, in parallel, each copying its shard's
    changes, in commit order, into the feed at `(SC, shard)`, with the stream's own sequence number, given after the
    write is durable. The lanes then grow with the table's throughput, as DynamoDB adds shards, not with its writers;
    but shards close and are replaced every few hours, and split, so that a cursor must follow their lineage; Streams
    keep 24 hours, so a Lambda more than a day behind loses changes, which could only be rebuilt by scanning the
    table; and each write is paid for twice;
  - **leased lanes:** a fixed number for each domain, e.g. 16, each with one writer at a time, which holds its lease by
    a conditional write, with a fencing number so that a writer which has lost it can write no more, and clients' puts
    go through their lane, so that a cursor is at most 16 numbers for each SC, whatever the clients; but the service is
    then not stateless, as a lane's holder keeps its lease and its counter.

### 8. Content which crosses domains is copied

- **Delivery copies.** A message from A, in org 1's domain, to B, in org 2's, is held in both: each organisation owns
  its copy, and its retention rules apply to that copy, so that org 2's compliance officer can have org 2's deleted
  after x days while org 1 keeps its own for the years its regulator demands. Pulling alone could meet neither duty.
- **A copy keeps its author's signature,** so that the receiving domain can prove who wrote it and cannot change it.
- **A store is not a domain:** it holds its own domain's records and copies of other domains', e.g. another domain's
  principals' key records, needed to check their signatures, kept up to date as they rotate keys.
- **Retention is enforced by each store's owner on everything it holds,** copies included. Deletion is local: a
  recall, or a request to delete, is a message to another domain, which it may honour or not, never an obligation
  the system enforces.

### 9. IDs are hashes of signed records

- **Every Entity's ID is the hash of its Identity Object,** and is trusted only once its signature is checked: a
  Principal's, by a key in it; a domain's, by its first Owner's, whose keys it holds; a kind's, by its author's.
  Uniqueness comes from the hash, with a nonce in each record so that two with the same name and key differ;
  authenticity comes from the signature. A bad actor may choose
  any key, a copy of someone else's public key too, but cannot sign with it, and copying a whole Principal's Identity
  Object only gives that same Principal, which they still cannot act as.
- **Only bruce-db's own types are kinds in hash keys:** `principal`, `directory`, the security records, and `data`.
  An app's Sequence is `<domain>/data/<ID>`, its ID the hash of a **descriptor**: its kind's ID and its parts, e.g. a
  meter's number, and its SC, see below.
- **A kind is a signed schema record,** e.g. `{name: "message", author: <PID>, nonce}`, its ID the hash of it, so that
  two authors' kinds of the same name differ, and two apps which agree on a format share one kind, e.g. two
  organisations exchanging messages. This namespaces by schema and its author, not by app.
- **A Sequence's SC is in its descriptor, so it is permanent:** it decides the feed's lane, see The feed, and a
  Sequence whose SC could change would have to appear in two SCs' feeds. Moving data to another SC is copying it to a
  new Sequence, which is checkable, and agrees with removal by rolling keys.
- **A Sequence's descriptor is its first record,** at sort key `-`, as a principal's record is, so that its ID can be
  checked and a browser can say what it is. An encrypted Sequence's descriptor is encrypted with it.
- **Keys tell a server nothing:** a server, a backup or DynamoDB sees `<domain>/data/<hex>`, not which apps are used or
  what is kept. The IDs spread evenly over DynamoDB's partitions, and prefix listing, which DynamoDB rules out, becomes
  impossible to depend on by accident.
- **What is enumerated needs an index Sequence,** e.g. a site's Sequences, as `principals` lists principals, since
  keys can no longer be listed by prefix.
- **A Sequence's ID is unique within its domain,** which is enough, as its hash key starts with the domain, and no one
  may write to it without Write in its SC.
- **The disk layout is uniform:** every `data` Sequence is split by its ID's first two hex characters, as principals
  are, in place of a list of split kinds.

### 10. An app's first use is its admission

- **An app cannot write to a domain until its installation's system principal is admitted,** as the capture devices
  are. That admission is the event at which the app announces itself, and the domain records the kinds it uses.
- **Local names** for apps and kinds, e.g. to tell two apps called "Smart Home" apart, are DirectoryRecords: names for
  a subject, given by an issuer, as built.

### 11. One state: every domain is founded

- **Creating a domain founds it:** the owner's name, the domain's name and their key file, made on first use and
  opened after. There is no unfounded domain; copies, demos and tests are founded too, by a throwaway founder whose key
  is kept with the copy.
- **The key file's password is remembered in the Keychain,** developing the item the app has already, so that a user
  who never shares sees a password only on first use. The password is kept, not the unlocked secret, so that there is
  one secret, one format and one way to unlock.
- **Moving a store made before domains** is migration code, to be removed once every existing store has moved.

### 12. Concurrent versions have one well-defined outcome

- **Every node orders versions the same way,** by their signed version, the time then the origin, as now, so that
  wherever two versions of an Object meet, the same one comes first.
- **An update carries the Absolute Hash of the version it was made from,** its base, beside the Relative Hash which
  all versions share. Two updates with the same base were made concurrently, and conflict.
- **Either the last update wins, or the second is rejected,** as the Object's kind says: last wins suits data whose
  source is the truth, e.g. a reading fetched again; rejecting suits what people edit, so that one person's change is
  not silently lost under another's.
- **A node may accept an update and later reject it,** when the conflicting update which comes first in the order
  arrives after it. Every node ends at the same outcome, as they all apply the same order.
- **A rejected update is resolved, not lost:** by merging the two, from a diff of the records, where the node can read
  them, i.e. they are not encrypted; or by sending it back to the user who made it, to resolve, where it cannot.

## What changes

### In design.md

- **Domains:** a domain is an owner, across apps, not one app's data; the Object Store's comparison stands.
- **The record model:** what design.md calls a DataSet, one hash key, is a Sequence, and a DataSet is the part of one
  which an app works with; Absolute and Relative Hashes, and every Object stored canonical. - **The record model's
  kinds:** a kind is no longer the part of a hash key after the domain for an app's data; app Sequences are
  `<domain>/data/<ID>`, with a descriptor, and kinds become signed schema records.
- **Membership:** the roles become independent permissions; "Admin … does what Write does" goes.
- **The SC graph:** the lowest role along a path becomes the intersection of permissions, the best of the paths their
  union; "a domain hides nothing from its owners" goes, as does the *Later* item on Admins managing users without
  reading their data, which this decides; the shape for an app loses the app SC.
- **DynamoDB:** the change feed's index is partitioned by SC and writer, not by domain and writer, with a summary
  partition for each SC, and the rule that a writer puts its changes in order; the item gets its SC.
- **Servers and clients:** the two kinds of server, trusted and untrusted, and enforcement by ACL within a domain.
- **New sections:** policies, copies across domains and retention, one store per node and the three ways of running
  it.
- **Layout:** `SPLIT_KINDS` gives way to splitting every `data` Sequence.

### In bruce-db's code

- `security.rs`: `Role` becomes a set of permissions, and `role_for` returns the set, intersected along paths and
  united across them. `sees_security_records`, any domain Admin, stays, as it is management.
- `security.rs`: founding stays as built, the domain's ID from its first Owner's keys, and each Principal's record
  naming its domain. Every SC but the root needs a parent with Admin, and Chinese Walls are checked when Read or
  Write is given.
- `key.rs`: hash keys for `data` Sequences from descriptors, and the uniform split.
- The file store: sharing between processes, see 7.

### In Bruce's Smart Home

- **Profiles become sites,** within one domain per owner: one founding, one principal in the key file, devices
  admitted once. The rename to "domain" built on 25 November was right for bruce-db and half wrong for the app.
- **No unfounded state,** see 11.
- **Owners are given Read explicitly** in the SCs they should see, when the app sets a domain up.
- **Its kinds, e.g. `tesla.snapshot`, become signed kind records and its Sequences opaque IDs,** by a domain upgrade,
  with index Sequences wherever the app lists keys today.
- **Its store moves** to the bruce-db location, see 7.
- **Jeff's migration** waits for these, so that it is done once.

## Open questions

From the definitions:

- **How a rejected update is sent back to its user** to resolve: as what, to which of their nodes, and how their app
  shows it.

Others:

- **Lanes for an SC with thousands of active writers:** stream shards or leased lanes, see The feed, when such an SC
  exists.
- **Where copies from another domain are kept** in a store, under which hash keys, and how a node tells its own records
  from copies.
- **Retention as a policy:** its form, and how each node applies it to what it holds.
- **Requests to delete across domains:** their form as messages.
- **"Active owners"**: what makes an owner inactive, and after how long.
- **Kinds' versions:** how a schema changes, and whether a new version is a new kind.
- **The local API's version,** as apps built against different versions of bruce-db share a store or a service.
- **Starting the service** where one is used: which app installs or starts it.
- **Encryption at rest** on trusted nodes: whether, and whose keys.

## Order of work

1. Bruce reviews and commits what step 12 built, so that there is a baseline.
2. design.md is updated to agree with these decisions.
3. bruce-db: permissions as sets; descriptors and `data` Sequences; the uniform split.
4. Smart Home: every domain founded, profiles as sites, the Keychain, and the domain upgrade to opaque IDs.
5. bruce-db: the store shared between processes through file locks, and its move to the bruce-db location.
6. The domain browser, the second app, sharing the store with Smart Home: principals, SCs, memberships, Sequences and
   records, read through descriptors.
7. Device replication, the replication between nodes which every domain with more than one node needs.
