// The contents of this file is licensed by its authors and copyright holders under the Apache
// License (Version 2.0), MIT license, or Mozilla Public License (Version 2.0), at your option. The
// contents of this file may not be copied, modified, or distributed except according to those
// terms. See the COPYRIGHT file at the top-level directory of this distribution for copies of these
// licenses and more information.

use nsobject::NSObject;
use objrs::objrs;

#[objrs(class, super = NSObject)]
#[link(name = "Foundation", kind = "framework")]
pub struct NSNotification;

#[objrs(impl)]
#[link(name = "Foundation", kind = "framework")]
impl NSNotification {}
