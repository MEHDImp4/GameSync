# Installer ou mettre à jour GameSync

Les builds de développement ciblent les systèmes x64. La chaîne de construction produit un installateur NSIS sur Windows et des paquets AppImage et Debian sur Linux. Les installations utilisent le même identifiant d’application pour conserver les réglages, les caches et le trousseau lors d’une mise à jour.

## Windows

Télécharger l’installateur `-setup.exe` et vérifier son SHA-256 avec `Get-FileHash -Algorithm SHA256 .\GameSync-setup.exe`, en remplaçant le nom par celui du fichier reçu. L’installateur s’exécute pour l’utilisateur courant. Si WebView2 est absent, l’installateur construit sous Windows le télécharge depuis Microsoft. L’installateur compilé depuis Linux inclut le runtime WebView2. Lancer GameSync depuis le menu Démarrer. Les builds de développement dépourvus d’un certificat de signature peuvent déclencher SmartScreen.

## Linux

Vérifier l’empreinte avec `sha256sum -c SHA256SUMS`. Sur Ubuntu ou Debian, ouvrir le paquet avec le gestionnaire de logiciels ou exécuter `sudo apt install ./GameSync_VERSION_amd64.deb` avec son nom exact. Pour l’AppImage, utiliser de préférence le paquet construit sur Ubuntu 22.04 par la CI. Conserver le fichier à un emplacement définitif, le rendre exécutable et l’ouvrir. Un service Secret Service accessible dans la session desktop est nécessaire pour l’appairage et la synchronisation.

Le paquet Debian et l’AppImage construits sur Ubuntu 22.04 ont été lancés dans des environnements Ubuntu 22.04 et 24.04 avec Xvfb.

Si le système ne dispose pas de FUSE compatible AppImage, utiliser `--appimage-extract-and-run` ou le paquet Debian lorsque disponible. Le démarrage automatique d’un AppImage pointe vers le fichier existant. Après un déplacement, désactiver puis réactiver cette préférence depuis le nouvel emplacement.

## Première synchronisation

1. Déployer le [serveur Docker](unraid.md) et générer le premier code d’administration avec `gamesync-server invite --admin` dans le conteneur. Le premier client peut ensuite générer des codes ordinaires depuis **Appareils**.
2. Dans **Paramètres**, saisir l’adresse HTTPS, le nom de l’appareil et son code.
3. Ajouter un jeu et confirmer le dossier de sauvegarde et l’exécutable. Avec le jeu fermé, synchroniser d’abord l’appareil qui possède la sauvegarde initiale.
4. Sur l’autre appareil, sélectionner le jeu dans **Jeux sur le serveur**, puis renseigner les dossiers locaux correspondant aux libellés affichés. En cas de contenu différent, choisir la bonne variante dans l’historique.
5. Activer la synchronisation et le démarrage en arrière-plan dans **Paramètres**.

Dans le détail d’un jeu, **Modifier les chemins** associe les mêmes dossiers logiques à de nouveaux emplacements locaux. GameSync compare alors le nouveau contenu à la version du serveur. **Mettre en pause** suspend le suivi de ce jeu. **Synchroniser et lancer** démarre l’exécutable configuré après une synchronisation réussie. Ce bouton est destiné aux jeux qui acceptent un lancement direct. Pour les jeux nécessitant Steam, Proton ou un autre lanceur, utiliser la synchronisation puis ouvrir le lanceur. Un lancement externe ne bénéficie pas d’un contrôle préalable garanti.

L’onglet **Activité** affiche le fichier en cours, les octets transférés et le débit moyen. Les 200 derniers événements de synchronisation restent consultables après un redémarrage. Les notifications système pour les nouveaux conflits et erreurs du moteur en arrière-plan s’activent dans **Paramètres**. Dans **Paramètres**, une limite non nulle ralentit les envois et les téléchargements du client. La progression reprend au dernier décalage disponible. Les octets déjà présents sont exclus du calcul du débit.

## Langue de l’interface

Dans **Paramètres → Apparence**, choisir le français ou l’anglais. Le choix est enregistré sur l’appareil. Le menu de la barre système utilise la nouvelle langue au prochain lancement. Les événements déjà enregistrés gardent leur texte d’origine. Certains diagnostics techniques sont affichés dans la langue fournie par le système ou le moteur.

## Mettre à jour

Quitter GameSync entièrement. Installer le nouvel NSIS ou le nouveau paquet Debian par-dessus la version existante. Pour l’AppImage, remplacer le fichier en gardant son chemin. Après redémarrage, contrôler l’état d’appairage et l’historique d’un jeu de test. Les mises à jour sont manuelles.

## Construire les installateurs

Le workflow GitHub Actions `GameSync builds` construit sur Linux et Windows natifs, exécute les tests du workspace et publie les paquets et leurs sommes SHA-256 comme artefacts de CI. La tâche Linux installe le paquet Debian sur le système de construction et vérifie le démarrage graphique du binaire installé et de l’AppImage. Le choix de la distribution de compilation détermine la compatibilité des bibliothèques embarquées dans l’AppImage. Pour produire un paquet Linux portable, utiliser une distribution de compilation au moins aussi ancienne que le plus ancien système visé. Depuis un système de compilation avec Rust stable, Node et les prérequis Tauri, exécuter dans `apps/desktop` les commandes `npm ci` et `npm run tauri -- build --ci -- --locked`.
