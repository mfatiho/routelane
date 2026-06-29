use crate::config::Language;

#[derive(Debug, Clone, Copy)]
pub struct Texts {
    pub inactive: &'static str,
    pub service_tooltip: &'static str,
    pub network_title: &'static str,
    pub network_description: &'static str,
    pub default_connection_title: &'static str,
    pub detecting: &'static str,
    pub exception_network_title: &'static str,
    pub interfaces_loading: &'static str,
    pub rules_title: &'static str,
    pub rules_description: &'static str,
    pub add_address_title: &'static str,
    pub address_placeholder: &'static str,
    pub add_rule_tooltip: &'static str,
    pub no_rules_title: &'static str,
    pub no_rules_subtitle: &'static str,
    pub route_test_title: &'static str,
    pub route_test_description: &'static str,
    pub test_address_title: &'static str,
    pub test_placeholder: &'static str,
    pub route_test_tooltip: &'static str,
    pub language_title: &'static str,
    pub language_description: &'static str,
    pub language_row_title: &'static str,
    pub language_row_subtitle: &'static str,
    pub default_not_found: &'static str,
    pub no_interfaces: &'static str,
    pub exception_subtitle: &'static str,
    pub exception_select_prompt: &'static str,
    pub activation_failed: &'static str,
    pub activation_failed_toast: &'static str,
    pub unselected: &'static str,
    pub address_word: &'static str,
    pub delete_rule_tooltip: &'static str,
    pub resolving: &'static str,
    pub tray_description: &'static str,
    pub tray_show: &'static str,
    pub tray_quit: &'static str,
    pub tray_tooltip: &'static str,
    pub routing_label: &'static str,
    pub settings_title: &'static str,
    pub menu_reset: &'static str,
    pub reset_dialog_heading: &'static str,
    pub reset_dialog_body: &'static str,
    pub reset_dialog_confirm: &'static str,
    pub reset_dialog_cancel: &'static str,
    pub used_network: &'static str,
    pub expected_network: &'static str,
    pub probe_samples: &'static str,
}

impl Language {
    pub fn texts(self) -> Texts {
        match self {
            Self::Tr => Texts {
                inactive: "Yönlendirme devre dışı",
                service_tooltip: "Yönlendirmeyi Etkinleştir / Devre Dışı Bırak",
                network_title: "Ağ Yapılandırması",
                network_description: "Varsayılan bağlantı otomatik algılanır. İstisna ağını seçin.",
                default_connection_title: "Varsayılan Bağlantı",
                detecting: "algılanıyor...",
                exception_network_title: "İstisna Ağı",
                interfaces_loading: "ağ arayüzleri yükleniyor",
                rules_title: "Yönlendirme İstisnaları",
                rules_description: "Bu adresler İstisna Ağı üzerinden yönlendirilir; diğer trafik değişmez.",
                add_address_title: "Adres Ekle",
                address_placeholder: "alan adı veya IP/CIDR  (örn: chatgpt.com, 8.8.8.8)",
                add_rule_tooltip: "Kural Ekle",
                no_rules_title: "Henüz kural eklenmedi",
                no_rules_subtitle: "Yukarıya bir alan adı veya IP adresi girin",
                route_test_title: "Rota Testi",
                route_test_description: "Bir adresin şu anda hangi ağ üzerinden çıktığını kontrol edin.",
                test_address_title: "Adres Test Et",
                test_placeholder: "gemini.google.com veya https://gemini.google.com/app",
                route_test_tooltip: "Rotayı Test Et",
                language_title: "Dil",
                language_description: "Arayüz dilini seçin.",
                language_row_title: "Arayüz Dili",
                language_row_subtitle: "Metinler hemen güncellenir",
                default_not_found: "varsayılan bağlantı bulunamadı",
                no_interfaces: "seçilebilir ağ arayüzü bulunamadı",
                exception_subtitle: "Eklenen adresler bu ağ üzerinden yönlendirilir",
                exception_select_prompt: "İstisna ağını seçin",
                activation_failed: "Etkinleştirme başarısız",
                activation_failed_toast: "Etkinleştirilemedi",
                unselected: "seçilmedi",
                address_word: "adres",
                delete_rule_tooltip: "Kuralı Sil",
                resolving: "çözümleniyor...",
                tray_description: "Seçili adresleri istisna ağına yönlendirir",
                tray_show: "Göster",
                tray_quit: "Çıkış",
                tray_tooltip: "Yönlendirme aracı",
                routing_label: "Yönlendirme",
                settings_title: "Tercihler",
                menu_reset: "Sıfırla",
                reset_dialog_heading: "Tüm Kurallar Silinecek",
                reset_dialog_body: "Bu işlem geri alınamaz. Tüm yönlendirme kuralları ve kernel durumu temizlenir.",
                reset_dialog_confirm: "Sıfırla",
                reset_dialog_cancel: "İptal",
                used_network: "Kullanılan Ağ",
                expected_network: "Beklenen Ağ",
                probe_samples: "Adresler",
            },
            Self::En => Texts {
                inactive: "Routing disabled",
                service_tooltip: "Enable / Disable Routing",
                network_title: "Network Configuration",
                network_description: "The default connection is detected automatically. Select the exception network.",
                default_connection_title: "Default Connection",
                detecting: "detecting...",
                exception_network_title: "Exception Network",
                interfaces_loading: "loading network interfaces",
                rules_title: "Routing Exceptions",
                rules_description: "These addresses are routed through the Exception Network; other traffic is unchanged.",
                add_address_title: "Add Address",
                address_placeholder: "domain or IP/CIDR  (e.g. chatgpt.com, 8.8.8.8)",
                add_rule_tooltip: "Add Rule",
                no_rules_title: "No rules added yet",
                no_rules_subtitle: "Enter a domain or IP address above",
                route_test_title: "Route Test",
                route_test_description: "Check which network an address currently uses.",
                test_address_title: "Test Address",
                test_placeholder: "gemini.google.com or https://gemini.google.com/app",
                route_test_tooltip: "Test Route",
                language_title: "Language",
                language_description: "Choose the interface language.",
                language_row_title: "Interface Language",
                language_row_subtitle: "Text updates immediately",
                default_not_found: "default connection not found",
                no_interfaces: "no selectable network interface found",
                exception_subtitle: "Added addresses are routed through this network",
                exception_select_prompt: "Select the exception network",
                activation_failed: "Activation failed",
                activation_failed_toast: "Could not enable",
                unselected: "not selected",
                address_word: "address",
                delete_rule_tooltip: "Delete Rule",
                resolving: "resolving...",
                tray_description: "Routes selected addresses through the exception network",
                tray_show: "Show",
                tray_quit: "Quit",
                tray_tooltip: "Routing tool",
                routing_label: "Routing",
                settings_title: "Preferences",
                menu_reset: "Reset",
                reset_dialog_heading: "All Rules Will Be Deleted",
                reset_dialog_body: "This action cannot be undone. All routing rules and kernel state will be cleared.",
                reset_dialog_confirm: "Reset",
                reset_dialog_cancel: "Cancel",
                used_network: "Used Network",
                expected_network: "Expected Network",
                probe_samples: "Addresses",
            },
        }
    }

    pub fn localized_status(self, status: &str) -> String {
        if self == Self::Tr {
            return status.to_owned();
        }

        if status == "Devre dışı" {
            return self.texts().inactive.to_owned();
        }

        if let Some(count) = status
            .strip_prefix("Aktif — ")
            .and_then(|value| value.split_whitespace().next())
        {
            return format!("Active - {} rule(s)", count);
        }

        status.to_owned()
    }

    pub fn default_gateway_subtitle(self, name: &str, gateway: &str) -> String {
        match self {
            Self::Tr => format!("{} - varsayılan ağ geçidi {}", name, gateway),
            Self::En => format!("{} - default gateway {}", name, gateway),
        }
    }

    pub fn route_sample_more(self, count: usize) -> String {
        match self {
            Self::Tr => format!(" +{} {}", count, self.texts().address_word),
            Self::En => format!(" +{} more {}", count, self.texts().address_word),
        }
    }
}
