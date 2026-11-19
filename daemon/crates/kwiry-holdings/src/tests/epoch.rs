//! S1 epochs: any epoch change is refused with `unexpected_epoch` while
//! leases run; an operator epoch expires an in-flight earlier-epoch cursor
//! (`410`, H.33) and the restarted list is refused too.

use super::*;
use crate::client::FetchFailure;

#[test]
fn h33_an_operator_epoch_mid_list_expires_the_cursor_and_the_restart_is_refused() {
    let (_dir, mut rig) = rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().add_active(PLAIN, "res-b");
    let report = applied(rig.holder().list_once());
    let posts = rig.authority.state().posts();
    rig.authority
        .state()
        .before_post(posts + 2, |state| state.new_epoch());
    assert_eq!(failed(rig.relist()), FetchFailure::UnexpectedEpoch);
    let statuses: Vec<u16> = rig
        .authority
        .state()
        .requests
        .iter()
        .skip(posts)
        .map(|r| r.status)
        .collect();
    assert_eq!(statuses, [200, 410, 200]);
    assert_eq!(rig.holder().state().high_water.as_ref(), Some(&report.rev));
    assert_eq!(rig.store.view().published.len(), 1);
}
