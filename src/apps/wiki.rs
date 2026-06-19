//! Bilingual per-tool help, shown in each tool's Wiki screen. Turkish is written
//! without diacritics (the display font is ASCII-only). Lines are pre-wrapped for
//! the 240px screen; the Wiki view scrolls them.

use crate::apps::hacking::Tool;
use crate::i18n;

/// The wiki body for a tool, in the active language.
pub fn get(t: Tool) -> &'static str {
    match t {
        Tool::WifiScan => i18n::t(EN_WIFI_SCAN, TR_WIFI_SCAN),
        Tool::WifiAnalyze => i18n::t(EN_WIFI_ANALYZE, TR_WIFI_ANALYZE),
        Tool::BleScan => i18n::t(EN_BLE_SCAN, TR_BLE_SCAN),
        Tool::Detector => i18n::t(EN_DETECTOR, TR_DETECTOR),
        Tool::BeaconSpam => i18n::t(EN_BEACON, TR_BEACON),
        Tool::ProbeFlood => i18n::t(EN_PROBE, TR_PROBE),
        Tool::BleSpam => i18n::t(EN_BLE_SPAM, TR_BLE_SPAM),
        Tool::EvilTwin => i18n::t(EN_EVIL_TWIN, TR_EVIL_TWIN),
        Tool::Deauth => i18n::t(EN_DEAUTH, TR_DEAUTH),
        Tool::Handshake => i18n::t(EN_HANDSHAKE, TR_HANDSHAKE),
        Tool::EvilPortal => i18n::t(EN_PORTAL, TR_PORTAL),
        Tool::NetScan => i18n::t(EN_NETSCAN, TR_NETSCAN),
    }
}

const EN_WIFI_SCAN: &str = "Scans nearby Wi-Fi access points.\nLists SSID, RSSI signal, channel,\nand encryption type per AP.\nUse it to map the RF environment\nand spot open or weak networks.\nPassive recon - reads beacons only.\nDefense: secure all APs with\nWPA2/WPA3, not open or WEP.";
const TR_WIFI_SCAN: &str = "Yakindaki Wi-Fi noktalarini tarar.\nHer AP icin SSID, RSSI sinyal,\nkanal ve sifreleme turunu listeler.\nRF ortamini haritalamak ve acik ya\nda zayif aglari gormek icin.\nPasif kesif - sadece beacon okur.\nSavunma: tum aglari WPA2/WPA3 ile\nkoru, acik veya WEP birakma.";

const EN_WIFI_ANALYZE: &str = "Same scan as a 2.4GHz histogram.\nShows AP count per channel 1-13.\nUse it to find crowded channels\nand pick a clear one.\nChannels 1/6/11 do not overlap.\nPassive - no packets are sent.\nDefense: spacing APs on 1/6/11\ncuts interference, not an attack.";
const TR_WIFI_ANALYZE: &str = "Ayni tarama 2.4GHz histogrami.\nKanal 1-13 basina AP sayisi.\nKalabalik kanallari bulup bos bir\nkanal secmek icin kullanilir.\nKanal 1/6/11 birbiriyle cakismaz.\nPasif - hicbir paket gonderilmez.\nSavunma: AP leri 1/6/11 e yaymak\nparaziti azaltir, saldiri degildir.";

const EN_BLE_SCAN: &str = "Passively lists nearby BLE devices.\nShows MAC address, RSSI, and name.\nUse it to inventory wearables, tags,\nbeacons and find unknown emitters.\nListen only - it does not connect.\nDefense: devices leak presence;\nuse random MACs and turn BLE off\nwhen it is not needed.";
const TR_BLE_SCAN: &str = "Yakindaki BLE cihazlarini listeler.\nMAC adresi, RSSI ve adi gosterir.\nGiyilebilir, etiket, beacon saymak\nve bilinmeyen yayinci bulmak icin.\nSadece dinler - baglanmaz.\nSavunma: cihazlar varligi sizdirir;\nrastgele MAC kullan, gereksizse\nBLE yi kapat.";

const EN_DETECTOR: &str = "Listens for deauth/disassoc frames.\nDetects a deauth ATTACK happening\nnearby in promiscuous mode.\nUse it to catch jamming or forced\ndisconnects of Wi-Fi clients.\nPurely defensive - sends nothing.\nDefense: a spike means someone is\nattacking; enable 802.11w (PMF).";
const TR_DETECTOR: &str = "Deauth/disassoc cercevelerini dinler.\nYakinda olan bir deauth SALDIRISINI\npromiscuous modda tespit eder.\nIstemcilerin zorla kopmasini veya\njamming i yakalamak icin kullanilir.\nTamamen savunmaci - bir sey yollamaz.\nSavunma: ani artis biri saldiriyor\ndemek; 802.11w (PMF) etkinlestir.";

const EN_BEACON: &str = "Floods fake Wi-Fi beacon frames.\nDozens of bogus access points appear\nin everyone's nearby Wi-Fi list.\nSSID names can be set or random.\nUse: demo how SSID lists are spoofed\nand how easily scanners are tricked.\nDefense: 802.11w / WIDS flags floods.";
const TR_BEACON: &str = "Sahte Wi-Fi beacon paketleri yayar.\nHerkesin listesinde onlarca sahte\nerisim noktasi gorunur.\nSSID adlari ayarli ya da rastgele.\nKullanim: SSID sahtekarligini ve\ntarayici aldatmasini gostermek.\nSavunma: 802.11w / WIDS tespit eder.";

const EN_PROBE: &str = "Sends many fake 802.11 probe reqs.\nMimics phantom clients seeking nets.\nVery noisy; pollutes air monitoring.\nUse: show how probe data is faked\nand how it hides device tracking.\nDefense: rate-limit probes, watch\nfor random MAC bursts.";
const TR_PROBE: &str = "Cok sayida sahte probe istegi yollar.\nAg arayan hayalet istemci taklit eder.\nCok gurultulu; izlemeyi kirletir.\nKullanim: probe verisinin nasil sahte\nyapildigini ve takibi gizledigini.\nSavunma: probe limitle, rastgele MAC\npatlamalarini izle.";

const EN_BLE_SPAM: &str = "Spams BLE advertising packets.\nTriggers pairing popups: Apple, Swift\nPair (Windows), Fast Pair (Android),\nor a flood of junk device names.\nUse: show how BLE ads cause popup\nspam and annoyance attacks.\nDefense: turn off BT in crowds; OS\npatches limit forced popups.";
const TR_BLE_SPAM: &str = "BLE reklam paketleri yayar.\nEslesme pencereleri tetikler: Apple,\nSwift Pair (Windows), Fast Pair\n(Android) ya da cop isim seli.\nKullanim: BLE reklamlarinin popup\nseli yapisini gostermek.\nSavunma: kalabalikta BT kapat;\nOS yamalari popupu sinirlar.";

const EN_EVIL_TWIN: &str = "Clones a real AP's SSID and channel.\nA rogue access point lure: victims\nmay join the fake one instead.\nUse: show why open SSIDs are unsafe\nand how creds get captured on twins.\nDefense: verify portal, use WPA3 and\nVPN; WIDS spots twin BSSIDs.";
const TR_EVIL_TWIN: &str = "Gercek bir AP'nin SSID/kanalini klonlar.\nSahte erisim noktasi tuzagi: kurbanlar\nsahtekine baglanabilir.\nKullanim: acik SSID'lerin tehlikesini\nve ikizde kimlik kapilmasini gostermek.\nSavunma: portal dogrula, WPA3 ve VPN\nkullan; WIDS ikizi bulur.";

const EN_DEAUTH: &str = "Sends 802.11 deauth frames to an AP.\nForces all clients to disconnect.\nA Wi-Fi denial-of-service attack.\nUse: test AP resilience; it pairs\nwith the Deauth Detector.\nDefense: enable 802.11w (PMF) to\nblock forged deauth frames.";
const TR_DEAUTH: &str = "AP'ye 802.11 deauth cercevesi yollar.\nTum istemcileri baglantidan atar.\nBir Wi-Fi hizmet engelleme saldirisi.\nKullanim: AP dayanikliligi testi;\nDeauth Detector ile eslesir.\nSavunma: 802.11w (PMF) ac, sahte\ndeauth cerceveleri engellenir.";

const EN_HANDSHAKE: &str = "Deauths a target so clients reconnect,\nthen sniffs the WPA 4-way handshake\n(EAPOL) during the reconnect.\nThat capture is cracked offline to\nrecover the Wi-Fi password.\nUse: show why weak passphrases fall.\nDefense: long random passphrase\nor WPA3 to resist cracking.";
const TR_HANDSHAKE: &str = "Hedefi deauth eder, istemci yeniden\nbaglanirken WPA 4-yollu handshake\n(EAPOL) yakalanir.\nBu kayit cevrimdisi kirilarak Wi-Fi\nparolasi elde edilir.\nKullanim: zayif parolanin nasil\ndustugunu gostermek.\nSavunma: uzun rasgele parola / WPA3.";

const EN_PORTAL: &str = "Runs a fake open AP with DHCP plus a\nDNS hijack: every site redirects to\none fake login page.\nWhatever the victim types is captured.\nUse: phishing / captive-portal demo.\nDefense: check HTTPS + SSID, never\nenter creds on open Wi-Fi.";
const TR_PORTAL: &str = "Sahte acik AP + DHCP + DNS yonlendirme:\nher site tek bir sahte giris\nsayfasina gider.\nKurbanin yazdigi her sey yakalanir.\nKullanim: oltalama / portal demosu.\nSavunma: HTTPS + SSID dogrula, acik\nWi-Fi'de parola girme.";

const EN_NETSCAN: &str = "Joins an open Wi-Fi, pulls a DHCP\nlease, then TCP connect-scans the\ngateway/router's common ports.\nFinds exposed services on the LAN.\nUse: map a network's surface.\nDefense: close unused ports, firewall\nand patch the router.";
const TR_NETSCAN: &str = "Acik Wi-Fi'ye baglanir, DHCP alir,\nsonra gateway/router'in yaygin\nportlarini TCP ile tarar.\nLAN'daki acik servisleri bulur.\nKullanim: ag yuzeyini haritalamak.\nSavunma: kullanilmayan portu kapat,\nrouter'a guvenlik duvari + yama.";
