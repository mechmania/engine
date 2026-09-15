use crate::game::{config::GameConfig, state::Mirror};
use serde::{Deserialize, Serialize};
use std::ops::{Index, IndexMut};

// The engine names the teams absolutely, a bot names them relative to itself. This
// file is shared verbatim between the two crates, so `TeamA`/`team_a` are rewritten
// at expansion time -- see `mm_macros::teams`.
#[cfg_attr(feature = "engine", mm_macros::teams(A, B))]
#[cfg_attr(feature = "client", mm_macros::teams(Me, Other))]
mod team_impl {
    #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, mm_macros::FfiMirror)]
    #[repr(u8)]
    pub enum Team {
        TeamA = 0,
        TeamB = 1,
    }

    // Both teams, in discriminant order. Lets code outside this module iterate the
    // teams without naming a variant -- the names are rewritten per build.
    pub const TEAMS: [Team; 2] = [Team::TeamA, Team::TeamB];

    impl Team {
        pub fn other_team(&self) -> Team {
            match self {
                Team::TeamA => Team::TeamB,
                Team::TeamB => Team::TeamA,
            }
        }

        // The discriminant, i.e. an index into `TEAMS`.
        pub const fn index(self) -> usize {
            self as usize
        }
    }

    impl Mirror for Team {
        fn mirror(&mut self, _: &GameConfig) {
            *self = self.other_team();
        }
    }

    #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug, mm_macros::FfiMirror)]
    #[repr(C)]
    pub struct TeamPair<T> {
        pub team_a: T,
        pub team_b: T,
    }

    impl<T> TeamPair<T> {
        pub fn new(team_a: T, team_b: T) -> Self {
            Self { team_a, team_b }
        }

        /// Exchanges the two halves. The field names are rewritten per build (see
        /// `mm_macros::teams`), so code outside a `#[teams(..)]` module cannot spell them
        /// and has to swap through here.
        pub fn swap(&mut self) {
            std::mem::swap(&mut self.team_a, &mut self.team_b);
        }
    }

    impl<T> Index<Team> for TeamPair<T> {
        type Output = T;
        fn index(&self, index: Team) -> &Self::Output {
            match index {
                Team::TeamA => &self.team_a,
                Team::TeamB => &self.team_b,
            }
        }
    }

    impl<T> IndexMut<Team> for TeamPair<T> {
        fn index_mut(&mut self, index: Team) -> &mut Self::Output {
            match index {
                Team::TeamA => &mut self.team_a,
                Team::TeamB => &mut self.team_b,
            }
        }
    }

    impl<T> IntoIterator for TeamPair<T> {
        type Item = T;
        type IntoIter = std::array::IntoIter<T, 2>;

        fn into_iter(self) -> Self::IntoIter {
            [self.team_a, self.team_b].into_iter()
        }
    }

    impl<'a, T> IntoIterator for &'a TeamPair<T> {
        type Item = &'a T;
        type IntoIter = std::array::IntoIter<&'a T, 2>;

        fn into_iter(self) -> Self::IntoIter {
            [&self.team_a, &self.team_b].into_iter()
        }
    }

    impl<'a, T> IntoIterator for &'a mut TeamPair<T> {
        type Item = &'a mut T;
        type IntoIter = std::array::IntoIter<&'a mut T, 2>;

        fn into_iter(self) -> Self::IntoIter {
            [&mut self.team_a, &mut self.team_b].into_iter()
        }
    }

    impl<T> TeamPair<T> {
        pub fn iter(&self) -> std::array::IntoIter<&T, 2> {
            [&self.team_a, &self.team_b].into_iter()
        }

        pub fn iter_mut(&mut self) -> std::array::IntoIter<&mut T, 2> {
            [&mut self.team_a, &mut self.team_b].into_iter()
        }
    }

    impl<T> Mirror for TeamPair<T>
    where
        T: Mirror,
    {
        fn mirror(&mut self, conf: &GameConfig) {
            self.swap();
            self.team_a.mirror(conf);
            self.team_b.mirror(conf);
        }
    }
}
