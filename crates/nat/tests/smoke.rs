//! Ручной smoke против реального роутера: UPnP-маппинг + снятие.
//! Запуск: `cargo test -p nat -- --ignored --nocapture`.
//! Без UPnP-шлюза тест падает на поиске — это ожидаемо в оффлайне.

#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать

use std::net::TcpListener;

/// Полный цикл: `map_tcp` на свободном порту → `unmap_tcp`.
#[test]
#[ignore = "нужен реальный UPnP-шлюз в сети"]
fn upnp_maps_and_unmaps_a_tcp_port() {
    // Свободный локальный порт для маппинга.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mapping = nat::map_tcp(port).expect("маппинг не удался: нет UPnP-шлюза?");
    println!(
        "маппинг: {port} → {}:{}",
        mapping.external_ip, mapping.external_port
    );
    assert!(!mapping.external_ip.is_unspecified());
    assert_ne!(mapping.external_port, 0);

    nat::unmap_tcp(mapping.external_port).expect("unmap не удался");
}
