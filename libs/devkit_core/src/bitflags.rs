/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[macro_export]
macro_rules! bitflags {
    (
        $(#[$outer:meta])*
        $vis:vis struct $Name:ident: $T:ty {
            $(
                $(#[$inner:meta])*
                const $Flag:ident = $value:expr;
            )*
        }
    ) => {
        $(#[$outer])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(transparent)]
        $vis struct $Name($T);

        #[allow(dead_code, deprecated, unused_doc_comments)]
        impl $Name {
            $(
                $(#[$inner])*
                pub const $Flag: Self = Self($value);
            )*

            pub const fn empty() -> Self {
                Self(0)
            }

            pub const fn all() -> Self {
                let mut bits: $T = 0;
                $(
                    $(#[$inner])*
                    {
                        bits |= Self::$Flag.0;
                    }
                )*
                Self(bits)
            }

            pub const fn bits(&self) -> $T {
                self.0
            }

            pub const fn from_bits(bits: $T) -> ::core::option::Option<Self> {
                if bits & !Self::all().0 == 0 {
                    ::core::option::Option::Some(Self(bits))
                } else {
                    ::core::option::Option::None
                }
            }

            pub const fn from_bits_truncate(bits: $T) -> Self {
                Self(bits & Self::all().0)
            }

            pub const fn from_bits_retain(bits: $T) -> Self {
                Self(bits)
            }

            pub const fn is_empty(&self) -> bool {
                self.0 == 0
            }

            pub const fn is_all(&self) -> bool {
                Self::all().0 & !self.0 == 0
            }

            pub const fn contains(&self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            pub const fn intersects(&self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            pub const fn union(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            pub const fn intersection(self, other: Self) -> Self {
                Self(self.0 & other.0)
            }

            pub const fn difference(self, other: Self) -> Self {
                Self(self.0 & !other.0)
            }

            pub const fn symmetric_difference(self, other: Self) -> Self {
                Self(self.0 ^ other.0)
            }

            pub const fn complement(self) -> Self {
                Self::from_bits_truncate(!self.0)
            }

            pub const fn insert(&mut self, other: Self) {
                self.0 |= other.0;
            }

            pub const fn remove(&mut self, other: Self) {
                self.0 &= !other.0;
            }

            pub const fn toggle(&mut self, other: Self) {
                self.0 ^= other.0;
            }

            pub const fn set(&mut self, other: Self, value: bool) {
                if value {
                    self.insert(other);
                } else {
                    self.remove(other);
                }
            }
        }

        impl ::core::ops::BitOr for $Name {
            type Output = Self;
            fn bitor(self, other: Self) -> Self {
                self.union(other)
            }
        }

        impl ::core::ops::BitAnd for $Name {
            type Output = Self;
            fn bitand(self, other: Self) -> Self {
                self.intersection(other)
            }
        }

        impl ::core::ops::BitXor for $Name {
            type Output = Self;
            fn bitxor(self, other: Self) -> Self {
                self.symmetric_difference(other)
            }
        }

        impl ::core::ops::Sub for $Name {
            type Output = Self;
            fn sub(self, other: Self) -> Self {
                self.difference(other)
            }
        }

        impl ::core::ops::Not for $Name {
            type Output = Self;
            fn not(self) -> Self {
                self.complement()
            }
        }

        impl ::core::ops::BitOrAssign for $Name {
            fn bitor_assign(&mut self, other: Self) {
                self.insert(other);
            }
        }

        impl ::core::ops::BitAndAssign for $Name {
            fn bitand_assign(&mut self, other: Self) {
                self.0 &= other.0;
            }
        }

        impl ::core::ops::BitXorAssign for $Name {
            fn bitxor_assign(&mut self, other: Self) {
                self.toggle(other);
            }
        }

        impl ::core::ops::SubAssign for $Name {
            fn sub_assign(&mut self, other: Self) {
                self.remove(other);
            }
        }

        #[allow(deprecated, unused_doc_comments)]
        impl ::core::fmt::Debug for $Name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(::core::stringify!($Name))?;
                f.write_str("(")?;
                let mut remaining: $T = self.0;
                let mut first = true;
                $(
                    $(#[$inner])*
                    {
                        let flag = Self::$Flag.0;
                        if flag != 0 && self.0 & flag == flag && remaining & flag != 0 {
                            if !first {
                                f.write_str(" | ")?;
                            }
                            first = false;
                            f.write_str(::core::stringify!($Flag))?;
                            remaining &= !flag;
                        }
                    }
                )*
                if remaining != 0 || first {
                    if !first {
                        f.write_str(" | ")?;
                    }
                    ::core::write!(f, "{:#x}", remaining)?;
                }
                f.write_str(")")
            }
        }
    };
}

#[cfg(test)]
mod tests {
    crate::bitflags! {
        struct Caps: u32 {
            const A = 1 << 0;
            const B = 1 << 1;
            const C = 1 << 4;
            const AB = Self::A.bits() | Self::B.bits();
        }
    }

    crate::bitflags! {
        #[derive(PartialOrd, Ord, Default)]
        pub(crate) struct Signed: i32 {
            const LOW = 1 << 1;
            const TOP = 1 << 31;
        }
    }

    const CONST_AC: Caps = Caps::A.union(Caps::C);

    #[test]
    fn construction() {
        assert_eq!(Caps::empty().bits(), 0);
        assert_eq!(Caps::all().bits(), 0x13);
        assert_eq!(Caps::from_bits(0x11), Some(Caps::A | Caps::C));
        assert_eq!(Caps::from_bits(0x40), None);
        assert_eq!(Caps::from_bits(0x41), None);
        assert_eq!(Caps::from_bits_truncate(0x41).bits(), 0x01);
        assert_eq!(Caps::from_bits_retain(0x41).bits(), 0x41);
        assert_eq!(CONST_AC.bits(), 0x11);
    }

    #[test]
    fn queries() {
        assert!(Caps::empty().is_empty());
        assert!(Caps::all().is_all());
        assert!(Caps::from_bits_retain(0xff).is_all());
        assert!(!(Caps::A | Caps::B).is_all());
        assert!(Caps::AB.contains(Caps::A));
        assert!(!Caps::A.contains(Caps::AB));
        assert!(Caps::A.contains(Caps::empty()));
        assert!(Caps::A.intersects(Caps::AB));
        assert!(!Caps::A.intersects(Caps::C));
        assert!(!Caps::A.intersects(Caps::empty()));
    }

    #[test]
    fn operators() {
        let abc = Caps::A | Caps::B | Caps::C;
        assert_eq!(abc & Caps::AB, Caps::AB);
        assert_eq!(abc ^ Caps::A, Caps::B | Caps::C);
        assert_eq!(abc - Caps::AB, Caps::C);
        assert_eq!(!Caps::A, Caps::B | Caps::C);
        assert_eq!((!Caps::from_bits_retain(0x41)).bits(), 0x12);
        assert_eq!((Caps::from_bits_retain(0x40) | Caps::A).bits(), 0x41);
        assert_eq!((Caps::from_bits_retain(0x41) - Caps::A).bits(), 0x40);

        let mut c = Caps::A;
        c |= Caps::C;
        assert_eq!(c, Caps::A | Caps::C);
        c &= Caps::C;
        assert_eq!(c, Caps::C);
        c ^= Caps::AB;
        assert_eq!(c, Caps::all());
        c -= Caps::B;
        assert_eq!(c, Caps::A | Caps::C);
    }

    #[test]
    fn mutation() {
        let mut c = Caps::empty();
        c.insert(Caps::AB);
        c.remove(Caps::A);
        assert_eq!(c, Caps::B);
        c.toggle(Caps::AB);
        assert_eq!(c, Caps::A);
        c.set(Caps::C, true);
        assert_eq!(c, Caps::A | Caps::C);
        c.set(Caps::A, false);
        assert_eq!(c, Caps::C);
    }

    #[test]
    fn debug() {
        assert_eq!(format!("{:?}", Caps::empty()), "Caps(0x0)");
        assert_eq!(format!("{:?}", Caps::C), "Caps(C)");
        assert_eq!(format!("{:?}", Caps::A | Caps::C), "Caps(A | C)");
        assert_eq!(format!("{:?}", Caps::AB), "Caps(A | B)");
        assert_eq!(format!("{:?}", Caps::from_bits_retain(0x40)), "Caps(0x40)");
        assert_eq!(format!("{:?}", Caps::from_bits_retain(0x51)), "Caps(A | C | 0x40)");
    }

    #[test]
    fn signed() {
        let s = Signed::LOW | Signed::TOP;
        assert_eq!(s.bits(), i32::MIN | 2);
        assert_eq!(Signed::from_bits(-1), None);
        assert_eq!(Signed::from_bits_truncate(-1), s);
        assert_eq!(!Signed::TOP, Signed::LOW);
        assert_eq!(Signed::default(), Signed::empty());
        assert!(Signed::TOP < Signed::LOW);
        assert_eq!(format!("{:?}", Signed::from_bits_retain(-1)), "Signed(LOW | TOP | 0x7ffffffd)");
    }
}
