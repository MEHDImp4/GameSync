# Architecture GameSync

## Composants

Le client Tauri utilise React pour l’interface. Les commandes Rust accèdent au trousseau système, aux dossiers approuvés et au serveur. SQLite conserve les réglages et l’activité locale. Le moteur de synchronisation est indépendant de Tauri. Les crates partagent les modèles, les règles de comparaison, le stockage BLAKE3, la découverte et le client HTTP.

Le serveur Axum conserve les appareils, les jeux et leurs révisions dans SQLite. Les octets sont stockés dans un volume persistant sous leur empreinte. Une instance correspond à un foyer. Tous ses appareils appairés peuvent synchroniser ses jeux. Seuls les administrateurs peuvent créer des invitations depuis l’application ou révoquer d’autres appareils.

## Synchronisation

Les appareils partagent des UUID de jeux et de racines. Chaque client mappe ces racines vers ses propres dossiers. Les manifestes contiennent des chemins relatifs et des empreintes. Les lecteurs de manifeste valident la portabilité des chemins et les collisions de noms.

Le moteur compare le manifeste local, le dernier manifeste connu du client et la révision courante du serveur. Le serveur fait avancer la révision courante uniquement lorsque la base de publication correspond encore à sa tête. Une divergence crée une variante consultable. Une résolution crée une nouvelle révision et clôt la variante dans la même transaction. Une clé d’idempotence protège la reprise après une réponse perdue.

Les objets sont transférés par blocs et vérifiés avant publication. Les envois reprennent au décalage confirmé par le serveur. Les téléchargements reprennent avec une requête Range.

## Écritures locales

La restauration préserve les fichiers d’origine dans le stockage local et écrit un journal avant les modifications. Chaque fichier entrant est préparé sur le volume de destination. GameSync vérifie le processus configuré et les fichiers concernés avant de remplacer ou de supprimer un fichier. La révision locale avance après vérification du manifeste final. Une reprise conserve tout fichier dont le contenu ne correspond ni à l’état initial ni à l’état cible.

Sous Linux, les opérations passent par des descripteurs de dossier. Sous Windows, le moteur refuse les points de réanalyse et maintient les dossiers parents ouverts durant l’opération. Un lancement externe concurrent reste possible. L’action **Synchroniser et lancer** permet à GameSync de contrôler l’ordre pour un exécutable compatible.

Voir le [contrat REST](../server/openapi.yaml) et la [procédure de déploiement](unraid.md).
