# Dépanner GameSync

## L’appairage ou la connexion échoue

Vérifier le serveur avec `curl -fsS http://127.0.0.1:8787/healthz` depuis son hôte. Sur les clients distants, vérifier que l’adresse HTTPS est accessible via le réseau privé. Un code d’appairage fonctionne une fois et expire après dix minutes. Régénérer un code depuis un appareil administrateur ou le conteneur. Si le trousseau Linux est verrouillé, le déverrouiller dans la session graphique avant de réessayer.

## Le jeu est en attente

Fermer le jeu et vérifier que l’exécutable configuré correspond au processus qui manipule ses sauvegardes. Un dossier indisponible ou un fichier modifié pendant la lecture bloque le transfert. Vérifier l’accès au dossier et l’espace disque. Une panne réseau laisse les fichiers locaux en place. GameSync reprend au retour du serveur.

## Deux machines ont progressé indépendamment

Ouvrir l’historique du jeu sur l’appareil qui signale le conflit et comparer la date, la machine et la taille. Appliquer la variante retenue ou une version antérieure. Les deux variantes peuvent rester en attente en fermant le détail. Si le serveur indique que la sélection a changé, actualiser l’historique avant de choisir à nouveau.

## Une restauration reste bloquée après un arrêt

Fermer le jeu et laisser GameSync reprendre. Si le journal signale une modification inattendue, copier le dossier de sauvegarde concerné dans un emplacement distinct avant d’intervenir. Préserver le répertoire de données GameSync contenant le journal et les objets locaux. Consulter **Activité** et les logs du conteneur pour identifier la révision et le fichier concernés.

## Informations de diagnostic

Noter la version du client, le système d’exploitation, l’heure et la révision concernée. Le tableau de bord montre les erreurs récentes du serveur et **Activité** conserve les derniers événements du client. Les logs du serveur sont disponibles avec `docker compose -f docker/compose.yaml logs --tail=100 gamesync`. Retirer les codes d’appairage, les jetons et les chemins personnels avant de partager un extrait.
