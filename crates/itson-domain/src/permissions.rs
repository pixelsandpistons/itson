//! Effective permission resolution.
//!
//! Order, matching the model Discord users already know:
//!
//! 1. The server owner has every permission.
//! 2. Base = everyone role, plus every role the member holds.
//! 3. `ADMINISTRATOR` in the base grants every permission.
//! 4. Channel overwrites apply in three layers, each deny-then-allow:
//!    the everyone role, then all of the member's roles combined, then the
//!    member individually. Later layers win.
//! 5. A timed-out member keeps only viewing and reading.
//! 6. Dependent flags fall away: without `VIEW_CHANNEL` nothing remains;
//!    without `SEND_MESSAGES` the member cannot attach or mention everyone;
//!    without `VOICE_CONNECT` no voice flag remains.

use itson_protocol::{Permissions, RoleId, UserId};

/// What a timed-out member keeps.
const TIMEOUT_KEEPS: Permissions = Permissions::VIEW_CHANNEL.union(Permissions::READ_HISTORY);

/// Allow and deny sets for one target in one channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overwrite {
    pub allow: Permissions,
    pub deny: Permissions,
}

impl Overwrite {
    fn apply(self, base: Permissions) -> Permissions {
        base.difference(self.deny).union(self.allow)
    }
}

/// Who a channel overwrite applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverwriteTarget {
    Role(RoleId),
    Member(UserId),
}

/// The member whose permissions are being resolved.
#[derive(Clone, Copy, Debug)]
pub struct MemberContext<'a> {
    pub user: UserId,
    pub is_owner: bool,
    /// Roles held, not including the implicit everyone role. IDs of deleted
    /// roles are ignored.
    pub roles: &'a [RoleId],
    pub timed_out: bool,
}

/// The server's role definitions.
#[derive(Clone, Copy, Debug)]
pub struct RoleTable<'a> {
    pub everyone: Permissions,
    pub roles: &'a [(RoleId, Permissions)],
}

impl RoleTable<'_> {
    fn get(&self, id: RoleId) -> Option<Permissions> {
        if id == RoleId::EVERYONE {
            return Some(self.everyone);
        }
        self.roles.iter().find(|(r, _)| *r == id).map(|(_, p)| *p)
    }
}

/// Everyone role plus held roles, or `None` for the owner and administrators,
/// who hold every permission.
fn role_union(member: &MemberContext<'_>, roles: &RoleTable<'_>) -> Option<Permissions> {
    if member.is_owner {
        return None;
    }
    let base = member
        .roles
        .iter()
        .filter_map(|id| roles.get(*id))
        .fold(roles.everyone, Permissions::union);
    (!base.contains(Permissions::ADMINISTRATOR)).then_some(base)
}

/// Server-wide permissions, used for server actions such as kicking or
/// managing roles. Channel rules (step 6) do not apply here.
pub fn base_permissions(member: &MemberContext<'_>, roles: &RoleTable<'_>) -> Permissions {
    match role_union(member, roles) {
        None => Permissions::ALL,
        Some(base) if member.timed_out => base & TIMEOUT_KEEPS,
        Some(base) => base,
    }
}

/// Permissions in one channel.
pub fn channel_permissions(
    member: &MemberContext<'_>,
    roles: &RoleTable<'_>,
    overwrites: &[(OverwriteTarget, Overwrite)],
) -> Permissions {
    let Some(mut perms) = role_union(member, roles) else {
        return Permissions::ALL;
    };
    for (target, ow) in overwrites {
        if *target == OverwriteTarget::Role(RoleId::EVERYONE) {
            perms = ow.apply(perms);
        }
    }

    let mut combined = Overwrite::default();
    for (target, ow) in overwrites {
        if let OverwriteTarget::Role(id) = target
            && *id != RoleId::EVERYONE
            && member.roles.contains(id)
        {
            combined.allow |= ow.allow;
            combined.deny |= ow.deny;
        }
    }
    perms = combined.apply(perms);

    for (target, ow) in overwrites {
        if *target == OverwriteTarget::Member(member.user) {
            perms = ow.apply(perms);
        }
    }

    finish(perms, member.timed_out)
}

fn finish(mut perms: Permissions, timed_out: bool) -> Permissions {
    if timed_out {
        perms &= TIMEOUT_KEEPS;
    }
    if !perms.contains(Permissions::VIEW_CHANNEL) {
        return Permissions::empty();
    }
    if !perms.contains(Permissions::SEND_MESSAGES) {
        perms = perms.difference(Permissions::ATTACH_FILES | Permissions::MENTION_EVERYONE);
    }
    if !perms.contains(Permissions::VOICE_CONNECT) {
        perms = perms.difference(
            Permissions::VOICE_SPEAK
                | Permissions::VOICE_MUTE_OTHERS
                | Permissions::VOICE_MOVE_MEMBERS,
        );
    }
    perms
}
